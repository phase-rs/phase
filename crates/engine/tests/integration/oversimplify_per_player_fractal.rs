//! Oversimplify (SOC/C21/DSC/PRM) — per-player Fractal token counter count.
//!
//! Oracle text:
//!   "Exile all creatures. Each player creates a 0/0 green and blue Fractal
//!    creature token and puts a number of +1/+1 counters on it equal to the
//!    total power of creatures they controlled that were exiled this way."
//!
//! Pre-fix bug: the dynamic-quantity clause was silently swallowed at parse
//! time (`SwallowedClause` / `DynamicQty`), so the Fractal token entered with
//! `enter_with_counters: []` and was a 0/0 — instantly dead to SBAs. Four
//! repairs land the card end-to-end:
//!
//!   1. Parser — `try_parse_put_counters_on_token_followup` now accepts the
//!      third-person `puts ` verb AND the `"a number of <type> counters on it
//!      equal to <quantity>"` body (delegating to the shared
//!      `parse_dynamic_counter_suffix_body`).
//!   2. Parser — historical quantities retain the exact earlier instruction's
//!      declared result address through token-counter absorption.
//!   3. Engine — the completed instruction publishes its captured departures.
//!   4. Engine — CR 608.2i: former type, controller and power are read from
//!      those departures, including lands that are no longer creatures.
//!
//! The token is created first and the counters are put on it afterwards
//! (CR 608.2c + CR 122.1): the Token is followed by a `ContinuationStep`
//! `PutCounter { target: LastCreated }` that carries the same player scope.
//! "Each player" governs both actions, so they are processed in the CR 608.2e
//! order: every player creates their Fractal (APNAP), and only then does every
//! player put counters on "it" — the token that player created, i.e. the
//! tokens that player owns (CR 111.2), bound per player when the second action
//! begins and held through any replacement-choice pause.
//!
//! The first test parses the verbatim Oracle text and drives the real resolver
//! (`resolve_ability_chain`) end-to-end: exile all creatures, then verify each
//! player's Fractal token receives the correct counter count equal to that
//! player's total exiled power. The remaining tests cast the card through the
//! real cast pipeline and answer every replacement-choice prompt, recording
//! the board at each prompt.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Oversimplify's verbatim Oracle text (MTGJSON).
const OVERSIMPLIFY: &str = "Exile all creatures. Each player creates a 0/0 green and blue Fractal creature token and puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way.";

/// Verbatim Oracle text (MTGJSON) of the replacement-effect fixtures.
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";

fn add_creature(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    // Ensure the battlefield list reflects the live creature.
    if !state.battlefield.contains(&id) {
        state.battlefield.push_back(id);
    }
    id
}

#[test]
fn oversimplify_per_player_fractal_counters_match_exiled_power() {
    let parsed = parse_oracle_text(
        OVERSIMPLIFY,
        "Oversimplify",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let definition = parsed
        .abilities
        .first()
        .expect("Oversimplify should parse a spell ability");

    // ----- Parser-level assertions (the four-task class repair). -----
    use engine::types::ability::{
        AggregateFunction, ControllerRef, Effect, ObjectProperty, PlayerFilter, QuantityExpr,
        QuantityRef, SubAbilityLink, TargetFilter, TypedFilter, ZoneChangeRecordSource,
    };
    let Effect::ChangeZoneAll { .. } = definition.effect.as_ref() else {
        panic!(
            "Oversimplify must start with ChangeZoneAll (exile all creatures), got {:?}",
            definition.effect
        );
    };
    let token_def = definition
        .sub_ability
        .as_deref()
        .expect("Oversimplify chains the per-player Token sub-ability");
    let Effect::Token {
        enter_with_counters,
        ..
    } = token_def.effect.as_ref()
    else {
        panic!(
            "Oversimplify's sub-ability must be a Token effect, got {:?}",
            token_def.effect
        );
    };
    assert!(
        enter_with_counters.is_empty(),
        "the Fractal token receives its counters by a later instruction, not as \
         entry counters: {enter_with_counters:?}"
    );
    let placement = token_def
        .sub_ability
        .as_deref()
        .expect("the counter placement is the Token's direct sub");
    assert_eq!(placement.sub_link, SubAbilityLink::ContinuationStep);
    // CR 608.2e: "Each player" is the subject of both actions, so the placement
    // carries the creation's player scope and runs as the second all-players
    // action rather than inside each player's creation.
    assert_eq!(token_def.player_scope, Some(PlayerFilter::All));
    assert_eq!(placement.player_scope, Some(PlayerFilter::All));
    assert_eq!(placement.player_scope, token_def.player_scope);
    let Effect::PutCounter {
        counter_type,
        count: count_expr,
        target: TargetFilter::LastCreated,
    } = placement.effect.as_ref()
    else {
        panic!(
            "the Token must be followed by PutCounter {{ LastCreated }} — pre-fix \
             the dynamic quantity was swallowed (DynamicQty SwallowedClause), got {:?}",
            placement.effect
        );
    };
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    let result = definition
        .declares_return_result
        .expect("the exact exile instruction declares its result");
    let expected_count = QuantityExpr::Ref {
        qty: QuantityRef::ZoneChangeAggregate {
            source: ZoneChangeRecordSource::InstructionResult { result },
            from: Some(Zone::Battlefield),
            to: Some(Zone::Exile),
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::ScopedPlayer),
                    ),
                ],
            },
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
        },
    };
    assert_eq!(
        count_expr, &expected_count,
        "the counter quantity retains the declared departure-result address"
    );

    // ----- Runtime end-to-end: cast and resolve. -----
    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(1000),
        PlayerId(0),
        "Oversimplify".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&source_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];

    // P0 controls a 4/4 and a 1/1 — total power 5.
    let p0_big = add_creature(&mut state, 101, PlayerId(0), "P0 Big", 4, 4);
    let p0_small = add_creature(&mut state, 102, PlayerId(0), "P0 Small", 1, 1);
    // P1 controls a single 3/3 — total power 3.
    let p1_mid = add_creature(&mut state, 103, PlayerId(1), "P1 Mid", 3, 3);
    // CR 109.4 + CR 608.2h: A *stolen* creature — owned by P0 but currently
    // controlled by P1 (Threaten-style). This exercises Task 4's LKI
    // controller-fallback end-to-end: post-exile, `obj.controller` would
    // reset toward owner (P0), but the look-back filter's "they controlled"
    // scoping must read the at-exile controller (P1) from the LKI cache.
    // Without the fallback, this 2/2 would count toward P0 (yielding P0:7,
    // P1:3) instead of P1 (correct: P0:5, P1:5).
    let stolen = add_creature(&mut state, 104, PlayerId(0), "Stolen 2/2", 2, 2);
    state.objects.get_mut(&stolen).unwrap().controller = PlayerId(1);

    let ability = build_resolved_from_def(definition, source_id, PlayerId(0));
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("Oversimplify must resolve without error");

    // Sanity: every battlefield creature left the battlefield (exiled).
    for victim in [p0_big, p0_small, p1_mid, stolen] {
        assert!(
            !state.battlefield.contains(&victim),
            "creature {victim:?} must be exiled off the battlefield"
        );
    }

    // Find each player's Fractal token. The per-player iteration creates one
    // token per player; the iterating player is the token's controller.
    let p0_token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_token && obj.controller == PlayerId(0) && obj.name == "Fractal")
        .expect("P0 must have a Fractal token on the battlefield");
    let p1_token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_token && obj.controller == PlayerId(1) && obj.name == "Fractal")
        .expect("P1 must have a Fractal token on the battlefield");

    // P0 controlled 4+1 = 5 power. P1 controlled 3 (P1 Mid) + 2 (the stolen
    // P0-owned 2/2) = 5 power — the stolen creature counts toward P1, NOT
    // P0, because "they controlled" reads the at-exile controller via LKI
    // (Task 4). If the LKI controller fallback regresses, this becomes
    // P0:7 / P1:3 and the asserts fail. Each token must also survive SBAs
    // (a 0/0 with N counters is N/N).
    let p0_counters = p0_token
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    let p1_counters = p1_token
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        p0_counters, 5,
        "P0's Fractal must receive 5 +1/+1 counters (4+1 power exiled — \
         the stolen 2/2 must NOT count for P0 since P1 controlled it at exile), \
         got {p0_counters}"
    );
    assert_eq!(
        p1_counters, 5,
        "P1's Fractal must receive 5 +1/+1 counters (3 own + 2 stolen \
         — the stolen P0-owned creature counts toward P1 because P1 was its \
         controller at exile per CR 109.4 LKI), got {p1_counters}"
    );
}

const OVERSIMPLIFY_SOURCE: &str = "Exile all creatures. Each player creates a 0/0 green and blue Fractal creature token and puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way.";
const AWAKENING_SOURCE: &str = "Until your next turn, all lands you control become 2/2 Elemental creatures with reach, indestructible, and haste. They're still lands.";

fn paid_departure_fixture(growth: bool) {
    use engine::game::scenario::GameScenario;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    let p0 = PlayerId(0);
    let p1 = PlayerId(1);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands = [ManaColor::Green, ManaColor::Blue, ManaColor::Green]
        .map(|color| scenario.add_basic_land(p0, color));
    let bears = scenario.add_creature(p0, "Grizzly Bears", 2, 2).id();
    let courser = scenario.add_creature(p1, "Centaur Courser", 3, 3).id();
    let exchange = scenario
        .add_spell_to_hand_from_oracle(
            p0,
            "Switcheroo",
            false,
            "Exchange control of two target creatures.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 4,
        })
        .id();
    let awakening = scenario
        .add_spell_to_hand_from_oracle(p0, "Sylvan Awakening", false, AWAKENING_SOURCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    let wipe = scenario
        .add_spell_to_hand_from_oracle(p0, "Oversimplify", false, OVERSIMPLIFY_SOURCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let bonus = growth.then(|| {
        scenario
            .add_spell_to_hand_from_oracle(
                p0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            })
            .id()
    });
    // Pool mana pays every printed cost; these three lands remain untapped witnesses.
    let colors = [
        ManaType::Blue,
        ManaType::Blue,
        ManaType::Green,
        ManaType::Green,
        ManaType::Green,
    ];
    let mana = colors
        .into_iter()
        .chain(std::iter::repeat_n(ManaType::Colorless, 9))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(p0, mana);
    for player in [p0, p1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut runner = scenario.build();
    runner
        .cast(exchange)
        .target_objects(&[bears, courser])
        .resolve();
    assert_eq!(runner.state().players[p0.0 as usize].mana_pool.total(), 9);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert_eq!(runner.state().objects[&bears].controller, p1);
    assert_eq!(runner.state().objects[&courser].controller, p0);
    assert_eq!(runner.state().objects[&bears].owner, p0);
    assert_eq!(runner.state().objects[&courser].owner, p1);
    runner.cast(awakening).resolve();
    assert_eq!(runner.state().players[p0.0 as usize].mana_pool.total(), 6);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    for land in lands {
        let object = &runner.state().objects[&land];
        assert!(object.card_types.core_types.contains(&CoreType::Creature));
        assert!(object.card_types.core_types.contains(&CoreType::Land));
        assert_eq!((object.power, object.toughness), (Some(2), Some(2)));
    }
    if let Some(bonus) = bonus {
        runner.cast(bonus).target_object(lands[0]).resolve();
        assert_eq!(runner.state().objects[&lands[0]].power, Some(5));
        assert_eq!(runner.state().players[p0.0 as usize].mana_pool.total(), 5);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
    }
    let outcome = runner.cast(wipe).resolve();
    // CR 608.2i: former control and departure characteristics determine the totals.
    for (player, expected) in [(p0, if growth { 12 } else { 9 }), (p1, 2)] {
        let tokens: Vec<_> = outcome
            .state()
            .battlefield
            .iter()
            .filter_map(|id| outcome.state().objects.get(id))
            .filter(|object| {
                object.is_token && object.name == "Fractal" && object.controller == player
            })
            .collect();
        assert_eq!(tokens.len(), 1);
        let token = tokens[0];
        assert_eq!(
            token.counters.get(&CounterType::Plus1Plus1),
            Some(&expected)
        );
        assert_eq!((token.base_power, token.base_toughness), (Some(0), Some(0)));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token
            .card_types
            .subtypes
            .iter()
            .any(|kind| kind == "Fractal"));
        assert!(token.color.contains(&ManaColor::Green));
        assert!(token.color.contains(&ManaColor::Blue));
        assert_eq!(token.owner, player);
    }
    for victim in lands.into_iter().chain([bears, courser]) {
        assert_eq!(outcome.state().objects[&victim].zone, Zone::Exile);
    }
    for land in lands {
        let object = &outcome.state().objects[&land];
        assert!(!object.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!((object.power, object.toughness), (None, None));
    }
    for spell in [exchange, awakening, wipe].into_iter().chain(bonus) {
        assert_eq!(outcome.state().objects[&spell].zone, Zone::Graveyard);
    }
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
    assert!(outcome.state().return_result_frames.is_empty());
    assert_eq!(
        outcome.state().players[p0.0 as usize].mana_pool.total(),
        usize::from(!growth)
    );
}

#[test]
fn oversimplify_paid_awakened_lands_use_departure_type_and_power() {
    paid_departure_fixture(false);
}

#[test]
fn oversimplify_departure_power_includes_additive_bonus() {
    paid_departure_fixture(true);
}

// Supplemental canonical building-block fixtures. The printed paid
// Awakening/Switcheroo/Oversimplify tests above remain the source proof.
fn departure_exile_definition(result: u32) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ReturnResultId};
    let effect: Effect = serde_json::from_value(serde_json::json!({
        "type": "ChangeZoneAll", "origin": "Battlefield", "destination": "Exile"
    }))
    .expect("canonical mass zone-change fixture");
    let mut definition = AbilityDefinition::new(AbilityKind::Spell, effect);
    // Use the established typed filter rather than depend on its wire internals.
    if let Effect::ChangeZoneAll { target, .. } = definition.effect.as_mut() {
        *target = engine::types::ability::TargetFilter::Typed(
            engine::types::ability::TypedFilter::creature(),
        );
    }
    definition.declares_return_result = Some(ReturnResultId(result));
    definition
}

fn departure_gain_definition(
    result: u32,
    player: PlayerId,
) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, AggregateFunction, ControllerRef, Effect, ObjectProperty,
        QuantityExpr, QuantityRef, ReturnResultId, TargetFilter, TypedFilter,
        ZoneChangeRecordSource,
    };
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            player: TargetFilter::SpecificPlayer { id: player },
            amount: QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate {
                    source: ZoneChangeRecordSource::InstructionResult {
                        result: ReturnResultId(result),
                    },
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Exile),
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::SpecificPlayer { id: player }),
                    ),
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                },
            },
        },
    )
}

fn departure_imported_coin_runner(
    flips: i32,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    departure_imported_coin_runner_with_population(flips, false, false)
}

fn departure_imported_coin_runner_with_population(
    flips: i32,
    foreign_seven: bool,
    empty: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if !empty {
        scenario.add_creature(P0, "Centaur Courser", 3, 3);
        scenario.add_creature(
            P0,
            if foreign_seven {
                "Rumbling Baloth"
            } else {
                "Grizzly Bears"
            },
            if foreign_seven { 4 } else { 2 },
            if foreign_seven { 4 } else { 2 },
        );
        scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    }
    scenario
        .add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![],
        });
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut reader = departure_gain_definition(7, P0);
    reader.sub_ability = Some(Box::new(departure_gain_definition(7, P1)));
    let mut definition = departure_exile_definition(7);
    definition.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: engine::types::ability::CoinFlipRun::Exactly {
                count: QuantityExpr::Fixed { value: flips },
            },
            win_effect: Some(Box::new(reader.clone())),
            lose_effect: Some(Box::new(reader)),
            flipper: TargetFilter::Controller,
        },
    )));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental addressed coin process", false)
        .with_ability_definition(definition)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    if foreign_seven {
        runner.state_mut().next_return_result_occurrence_id = 19;
        runner.state_mut().next_return_result_process_id = 37;
    }
    (runner, spell)
}

#[test]
fn departure_coin_keep_restores_exact_import() {
    use engine::game::scenario::{P0, P1};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, spell) = departure_imported_coin_runner(1);
    runner.cast(spell).resolve();
    let WaitingFor::CoinFlipKeepChoice {
        player,
        results,
        keep_count,
    } = &runner.state().waiting_for
    else {
        panic!("real Thumb keep prompt")
    };
    assert_eq!(*player, P0);
    assert_eq!(results.len(), 2);
    assert_eq!(*keep_count, 1);
    assert_eq!(runner.state().players[0].life, 20);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    let saved = serde_json::to_string(runner.state()).unwrap();
    *runner.state_mut() =
        serde_json::from_str(&saved).expect("reached imported owner survives reload");
    let response = runner
        .act(GameAction::SelectCoinFlips {
            keep_indices: vec![1],
        })
        .unwrap();
    assert_eq!(
        response
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::CoinFlipped { .. }))
            .count(),
        1
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, 25);
    assert_eq!(runner.state().players[P1.0 as usize].life, 22);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn departure_coin_repause_reload_preserves_import() {
    use engine::game::scenario::P0;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, spell) = departure_imported_coin_runner(2);
    runner.cast(spell).resolve();
    for iteration in 0..2 {
        let WaitingFor::CoinFlipKeepChoice {
            player,
            results,
            keep_count,
        } = &runner.state().waiting_for
        else {
            panic!("each logical flip parks its own public keep prompt")
        };
        assert_eq!(*player, P0);
        assert_eq!(results.len(), 2);
        assert_eq!(*keep_count, 1);
        assert_eq!(runner.state().return_result_frames.len(), 1);
        let saved = serde_json::to_string(runner.state()).unwrap();
        *runner.state_mut() =
            serde_json::from_str(&saved).expect("re-pause retains exact imported owner");
        let response = runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![1],
            })
            .unwrap();
        assert_eq!(
            response
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::CoinFlipped { .. }))
                .count(),
            1
        );
        assert_eq!(runner.state().players[0].life, 20 + 5 * (iteration + 1));
        assert_eq!(runner.state().players[1].life, 20 + 2 * (iteration + 1));
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn departure_same_process_sum_reads_two_local_declarations() {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{ControllerRef, Effect, QuantityExpr, TargetFilter, TypedFilter};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let first_courser = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
    let second_courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut a = departure_exile_definition(7);
    let mut b = departure_exile_definition(11);
    for (definition, player) in [(&mut a, P0), (&mut b, P1)] {
        let Effect::ChangeZoneAll { target, .. } = definition.effect.as_mut() else {
            unreachable!()
        };
        *target = TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::SpecificPlayer { id: player }),
        );
    }
    let mut reader = departure_gain_definition(7, P0);
    let other_reader = departure_gain_definition(11, P1);
    let Effect::GainLife {
        amount: other_amount,
        ..
    } = other_reader.effect.as_ref()
    else {
        unreachable!()
    };
    let Effect::GainLife { amount, .. } = reader.effect.as_mut() else {
        unreachable!()
    };
    *amount = QuantityExpr::Sum {
        exprs: vec![amount.clone(), other_amount.clone()],
    };
    b.sub_ability = Some(Box::new(reader));
    a.sub_ability = Some(Box::new(b));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental same-process local sum", false)
        .with_ability_definition(a)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_eq!(runner.state().players[0].life, 28);
    assert_eq!(runner.state().players[1].life, 20);
    for victim in [bears, first_courser, second_courser] {
        assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn departure_final_chooser_local_child_keeps_outer_tail() {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, ControllerRef, Effect, PlayerFilter,
        QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    for paused in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let outer_courser = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
        let inner_courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        if paused {
            scenario
                .add_artifact_from_oracle(
                    P0,
                    "Krark's Thumb",
                    "If you would flip a coin, instead flip two coins and ignore one.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                });
        }
        for player in [P0, P1] {
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Library witness");
            }
        }
        let mut a = departure_exile_definition(7);
        let mut b = departure_exile_definition(11);
        for (definition, player) in [(&mut a, P0), (&mut b, P1)] {
            let Effect::ChangeZoneAll { target, .. } = definition.effect.as_mut() else {
                unreachable!()
            };
            *target = TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::SpecificPlayer { id: player }),
            );
        }
        let inner_reader = departure_gain_definition(11, P1);
        b.sub_ability = Some(Box::new(if paused {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    flips: engine::types::ability::CoinFlipRun::Exactly {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    win_effect: Some(Box::new(inner_reader.clone())),
                    lose_effect: Some(Box::new(inner_reader)),
                    flipper: TargetFilter::Controller,
                },
            )
        } else {
            inner_reader
        }));
        let mut choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp), b],
                kind: ChooseOneOfKind::Ordinary,
            },
        );
        choice.sub_ability = Some(Box::new(departure_gain_definition(7, P0)));
        a.sub_ability = Some(Box::new(choice));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental local choice with outer tail", false)
            .with_ability_definition(a)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        let WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } = &runner.state().waiting_for
        else {
            panic!("actual public branch prompt")
        };
        assert_eq!(*player, P0);
        assert_eq!(branches.len(), 2);
        assert_eq!(
            runner.state().objects[&inner_courser].zone,
            Zone::Battlefield
        );
        let saved = serde_json::to_string(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_str(&saved)
            .expect("future complete local child is valid before it runs");
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        if paused {
            let WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } = &runner.state().waiting_for
            else {
                panic!("inner local process owns the Thumb pause")
            };
            assert_eq!(*player, P0);
            assert_eq!(results.len(), 2);
            assert_eq!(*keep_count, 1);
            assert_eq!(
                runner.state().players[0].life,
                20,
                "outer tail waits outside child frames"
            );
            assert_eq!(runner.state().players[1].life, 20);
            assert_eq!(runner.state().return_result_frames.len(), 2);
            let saved = serde_json::to_string(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_str(&saved)
                .expect("both local child and outer owner survive reload");
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
        }
        assert_eq!(
            runner.state().players[0].life,
            25,
            "outer A is five, restored exactly once"
        );
        assert_eq!(runner.state().players[1].life, 23, "local B is three");
        for victim in [bears, outer_courser, inner_courser] {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
    }
}

// Supplemental canonical fixture: these composed abilities are not printed Oracle text.
fn departure_zone_entry_prompt(
    foreign_seven: bool,
    empty: bool,
    predicate: bool,
    gate: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    departure_zone_entry_prompt_with_read(true, foreign_seven, empty, predicate, gate)
}

fn departure_zone_entry_prompt_with_read(
    addressed: bool,
    foreign_seven: bool,
    empty: bool,
    predicate: bool,
    gate: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    departure_zone_entry_prompt_with_members(addressed, foreign_seven, empty, predicate, gate, 0)
}

fn departure_zone_entry_prompt_with_members(
    addressed: bool,
    foreign_seven: bool,
    empty: bool,
    predicate: bool,
    gate: bool,
    members: u8,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, Effect, FilterProp, TargetChoiceTiming,
        TargetFilter, TypedFilter,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut departed = Vec::new();
    if !empty {
        departed.push(scenario.add_creature(P0, "Centaur Courser", 3, 3).id());
        departed.push(
            scenario
                .add_creature(
                    P0,
                    if foreign_seven {
                        "Rumbling Baloth"
                    } else {
                        "Grizzly Bears"
                    },
                    if foreign_seven { 4 } else { 2 },
                    if foreign_seven { 4 } else { 2 },
                )
                .id(),
        );
        departed.push(scenario.add_creature(P1, "Grizzly Bears", 2, 2).id());
    }
    let first = if members == 2 {
        let aura = scenario
            .add_enchantment_from_oracle(P0, "Supplemental canonical conditional-entry Aura", "")
            .with_subtypes(vec!["Aura"])
            .with_keyword(engine::types::keywords::Keyword::Enchant(
                TargetFilter::Typed(TypedFilter::new(
                    engine::types::ability::TypeFilter::Artifact,
                )),
            ))
            .id();
        scenario.add_artifact_from_oracle(P0, "Supplemental entry host A", "");
        scenario.add_artifact_from_oracle(P0, "Supplemental entry host B", "");
        aura
    } else {
        scenario
            .add_creature_to_graveyard(P0, "Centaur Courser", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![engine::types::mana::ManaCostShard::Green],
            })
            .id()
    };
    let second = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![engine::types::mana::ManaCostShard::Green],
        })
        .id();
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, P0).effect else {
        unreachable!()
    };
    if !addressed {
        amount = engine::types::ability::QuantityExpr::Fixed { value: 1 };
    }
    let mut threshold = TypedFilter::default();
    threshold.properties.push(FilterProp::Cmc {
        comparator: Comparator::LT,
        value: amount.clone(),
    });
    let threshold = TargetFilter::Typed(threshold);
    let mut effect: Effect = serde_json::from_value(serde_json::json!({
        "type": "ChangeZone", "origin": "Graveyard", "destination": "Battlefield"
    }))
    .unwrap();
    let Effect::ChangeZone {
        target,
        conditional_enter_with_counters,
        enters_modified_if,
        enter_tapped,
        ..
    } = &mut effect
    else {
        unreachable!()
    };
    *target = if members == 2 {
        TargetFilter::Or {
            filters: vec![
                TargetFilter::SpecificObject { id: first },
                TargetFilter::SpecificObject { id: second },
            ],
        }
    } else {
        TargetFilter::Typed(TypedFilter::creature())
    };
    conditional_enter_with_counters.push((
        if predicate {
            threshold.clone()
        } else {
            TargetFilter::Any
        },
        CounterType::Plus1Plus1,
        amount,
    ));
    if gate {
        *enters_modified_if = Some(threshold);
        *enter_tapped = engine::types::zones::EtbTapState::Tapped;
    }
    let mut entry = AbilityDefinition::new(AbilityKind::Spell, effect);
    entry.target_choice_timing = TargetChoiceTiming::Resolution;
    if members != 0 {
        entry.multi_target = Some(engine::types::ability::MultiTargetSpec::fixed(2, 2));
    }
    let root = if addressed {
        let mut root = departure_exile_definition(7);
        root.sub_ability = Some(Box::new(entry));
        root
    } else {
        entry
    };
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental departure entry process", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    if members == 2 {
        let mut setup = vec![];
        engine::game::zones::move_to_zone(runner.state_mut(), first, Zone::Graveyard, &mut setup);
    }
    if members != 0 {
        use engine::types::ability::{AbilityCost, ReplacementDefinition, ReplacementMode};
        use engine::types::replacements::ReplacementEvent;
        for candidate in [first, second] {
            if members == 2 && candidate == first {
                continue;
            }
            let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .mode(ReplacementMode::MayCost {
                    cost: AbilityCost::PayLife {
                        amount: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::NoOp,
                    ))),
                });
            let object = runner.state_mut().objects.get_mut(&candidate).unwrap();
            object.replacement_definitions.push(replacement.clone());
            std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
        }
    }
    if foreign_seven {
        // A genuinely executed foreign invocation, with valid distinct allocator domains.
        runner.state_mut().next_return_result_occurrence_id = 19;
        runner.state_mut().next_return_result_process_id = 37;
    }
    runner.cast(spell).commit();
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::EffectZoneChoice { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ));
        runner
            .act(engine::types::actions::GameAction::PassPriority)
            .unwrap();
    }
    let engine::types::game_state::WaitingFor::EffectZoneChoice {
        player,
        cards,
        return_result_occurrence,
        return_result_process,
        conditional_enter_with_counters,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("real final zone choice owns its unresolved result")
    };
    assert_eq!(*player, P0);
    assert_eq!(cards.as_slice(), &[first, second]);
    assert_eq!(conditional_enter_with_counters.len(), 1);
    if addressed {
        let occurrence = return_result_occurrence.expect("actual producer owns O");
        let process = return_result_process.expect("executing instruction owns independent P");
        let frame = &runner.state().return_result_frames[&occurrence];
        assert_eq!(frame.process, Some(process));
        let engine::types::game_state::InstructionResult::ZoneChanges(records) =
            &frame.results[&engine::types::ability::ReturnResultId(7)]
        else {
            panic!("real completed departure result, including genuine empty");
        };
        assert_eq!(records.len(), if empty { 0 } else { 3 });
        assert_eq!(
            records
                .iter()
                .filter(|record| record.controller == P0)
                .map(|record| record.power.unwrap())
                .sum::<i32>(),
            if empty {
                0
            } else if foreign_seven {
                7
            } else {
                5
            }
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.controller == P1)
                .map(|record| record.power.unwrap())
                .sum::<i32>(),
            if empty { 0 } else { 2 }
        );
        for id in departed {
            let record = records
                .iter()
                .find(|record| record.object_id == id)
                .expect("every actual victim recorded once");
            assert_eq!(record.from_zone, Some(Zone::Battlefield));
            assert_eq!(record.to_zone, Zone::Exile);
            assert!(record.trigger_source_context.is_some());
            assert_eq!(record.owner, record.controller);
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
        }
    } else {
        assert!(return_result_occurrence.is_none());
        assert!(return_result_process.is_none());
        assert!(runner.state().return_result_frames.is_empty());
    }
    assert!(
        runner
            .state()
            .resolution_stack
            .iter()
            .all(|frame| !matches!(
                frame,
                engine::types::resolution::ResolutionFrame::AbilityContinuation(_)
            )),
        "final prompt has no continuation authority"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    (runner, spell, [first, second])
}

fn departure_assert_entry_complete(
    runner: &engine::game::scenario::GameRunner,
    spell: engine::types::identifiers::ObjectId,
    selected: engine::types::identifiers::ObjectId,
    unselected: engine::types::identifiers::ObjectId,
    counters: u32,
) {
    use engine::types::game_state::WaitingFor;
    assert_eq!(runner.state().objects[&selected].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&unselected].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&selected]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        counters
    );
    // CR 122.1a: the selected real Bears' five +1/+1 counters make it 7/7.
    assert_eq!(
        runner.state().objects[&selected].power,
        Some(2 + counters as i32)
    );
    assert_eq!(
        runner.state().objects[&selected].toughness,
        Some(2 + counters as i32)
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn departure_retained_readers_restore_and_retire_exact_occurrence() {
    use engine::game::scenario::{P0, P1};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for empty in [false, true] {
        for (predicate, gate) in [(false, false), (true, false), (false, true)] {
            let (mut runner, spell, [first, second]) =
                departure_zone_entry_prompt(false, empty, predicate, gate);
            for projected in [
                engine::game::visibility::filter_state_for_viewer(runner.state(), P0),
                engine::game::visibility::filter_state_for_viewer(runner.state(), P1),
                engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
            ] {
                let WaitingFor::EffectZoneChoice {
                    player,
                    return_result_occurrence,
                    return_result_process,
                    ..
                } = projected.waiting_for
                else {
                    panic!("public choice spine remains")
                };
                assert_eq!(player, P0);
                assert!(return_result_occurrence.is_none());
                assert!(return_result_process.is_none());
                assert!(projected.return_result_frames.is_empty());
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("initial sole owner survives canonical reload");
            runner
                .act(GameAction::SelectCards {
                    cards: vec![second],
                })
                .unwrap();
            departure_assert_entry_complete(
                &runner,
                spell,
                second,
                first,
                if empty { 0 } else { 5 },
            );
            assert_eq!(runner.state().objects[&second].tapped, gate && !empty);
        }
    }
}

#[test]
fn departure_retained_reader_invalid_sources_fail_before_scalar() {
    use engine::types::ability::ReturnResultId;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, RevealedInstructionResult, WaitingFor};
    let (mut foreign, foreign_spell, [foreign_first, foreign_second]) =
        departure_zone_entry_prompt(true, false, false, false);
    let WaitingFor::EffectZoneChoice {
        return_result_occurrence: Some(foreign_occurrence),
        ..
    } = foreign.state().waiting_for
    else {
        unreachable!()
    };
    let foreign_frame = foreign.state().return_result_frames[&foreign_occurrence].clone();
    foreign
        .act(GameAction::SelectCards {
            cards: vec![foreign_second],
        })
        .unwrap();
    departure_assert_entry_complete(&foreign, foreign_spell, foreign_second, foreign_first, 7);
    for (predicate, gate) in [(false, false), (true, false), (false, true)] {
        let (positive, spell, [first, second]) =
            departure_zone_entry_prompt(false, false, predicate, gate);
        let WaitingFor::EffectZoneChoice {
            return_result_occurrence: Some(original),
            return_result_process: Some(expected),
            ..
        } = positive.state().waiting_for
        else {
            unreachable!()
        };
        assert_ne!(foreign_frame.process, Some(expected));
        for fault in 0..8 {
            let mut hostile = positive.state().clone();
            match fault {
                0 => {
                    hostile.return_result_frames.remove(&original);
                    hostile
                        .return_result_frames
                        .insert(foreign_occurrence, foreign_frame.clone());
                    hostile.next_return_result_occurrence_id = foreign_occurrence.0 + 1;
                    hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                    let WaitingFor::EffectZoneChoice {
                        return_result_occurrence,
                        ..
                    } = &mut hostile.waiting_for
                    else {
                        unreachable!()
                    };
                    *return_result_occurrence = Some(foreign_occurrence);
                }
                1 => {
                    hostile.return_result_frames.clear();
                }
                2 => {
                    hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .remove(&ReturnResultId(7));
                }
                3 => {
                    hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .insert(
                            ReturnResultId(7),
                            InstructionResult::RevealedCards(RevealedInstructionResult {
                                revealed_cards: vec![],
                            }),
                        );
                }
                4 => {
                    let WaitingFor::EffectZoneChoice {
                        return_result_process,
                        ..
                    } = &mut hostile.waiting_for
                    else {
                        unreachable!()
                    };
                    *return_result_process = None;
                }
                5 => {
                    let WaitingFor::EffectZoneChoice {
                        return_result_occurrence,
                        ..
                    } = &mut hostile.waiting_for
                    else {
                        unreachable!()
                    };
                    *return_result_occurrence = None;
                }
                6 => {
                    let InstructionResult::ZoneChanges(records) = hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .get_mut(&ReturnResultId(7))
                        .unwrap()
                    else {
                        unreachable!()
                    };
                    records[0].trigger_source_context = None;
                }
                7 => {
                    hostile.resolving_stack_entry = None;
                }
                _ => unreachable!(),
            }
            let saved = serde_json::to_value(&hostile).unwrap();
            let error = serde_json::from_value::<GameState>(saved)
                .unwrap_err()
                .to_string();
            if fault == 0 || fault == 4 {
                assert!(
                    error.contains("instruction-result process owner mismatch"),
                    "{error}"
                );
            }
            let mut reached = engine::game::scenario::GameRunner::from_state(hostile);
            let before = serde_json::to_value(reached.state()).unwrap();
            let error = reached
                .act(GameAction::SelectCards {
                    cards: vec![second],
                })
                .unwrap_err()
                .to_string();
            if fault == 0 || fault == 4 {
                assert!(
                    error.contains("instruction-result process owner mismatch"),
                    "{error}"
                );
            }
            if fault == 7 {
                assert!(error.contains("zone entry evaluation context"), "{error}");
            }
            assert_eq!(
                serde_json::to_value(reached.state()).unwrap(),
                before,
                "hostile response cannot mutate choice, members, counters, zones or events"
            );
        }
        let mut intact = positive;
        intact
            .act(GameAction::SelectCards {
                cards: vec![second],
            })
            .unwrap();
        departure_assert_entry_complete(&intact, spell, second, first, 5);
    }
}

#[test]
fn departure_final_zone_choice_without_history_keeps_ordinary_entry() {
    use engine::game::scenario::{P0, P1};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, spell, [first, second]) =
        departure_zone_entry_prompt_with_read(false, false, false, false, false);
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() =
        serde_json::from_value(saved).expect("ordinary no-read prompt is legacy compatible");
    for projected in [
        engine::game::visibility::filter_state_for_viewer(runner.state(), P0),
        engine::game::visibility::filter_state_for_viewer(runner.state(), P1),
        engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
    ] {
        assert!(matches!(
            projected.waiting_for,
            WaitingFor::EffectZoneChoice {
                return_result_occurrence: None,
                return_result_process: None,
                ..
            }
        ));
        assert!(projected.return_result_frames.is_empty());
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .unwrap();
    departure_assert_entry_complete(&runner, spell, second, first, 1);
}

#[test]
fn departure_coin_saved_process_rejects_foreign_and_missing_results_before_keep() {
    use engine::game::scenario::{GameRunner, P0, P1};
    use engine::types::ability::ReturnResultId;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, RevealedInstructionResult, WaitingFor};
    let (mut foreign, foreign_spell) =
        departure_imported_coin_runner_with_population(1, true, false);
    foreign.cast(foreign_spell).resolve();
    assert!(matches!(
        foreign.state().waiting_for,
        WaitingFor::CoinFlipKeepChoice { .. }
    ));
    let foreign_pending = foreign.state().active_coin_flip_frame().unwrap();
    let foreign_occurrence = foreign_pending.return_result_occurrence.unwrap();
    let foreign_frame = foreign.state().return_result_frames[&foreign_occurrence].clone();
    assert_eq!(
        foreign_pending.expected_return_result_process,
        foreign_frame.process
    );
    foreign
        .act(GameAction::SelectCoinFlips {
            keep_indices: vec![1],
        })
        .unwrap();
    assert_eq!(foreign.state().players[0].life, 27);
    assert_eq!(foreign.state().players[1].life, 22);
    assert!(foreign.state().return_result_frames.is_empty());
    let (mut positive, spell) = departure_imported_coin_runner(1);
    positive.cast(spell).resolve();
    let owner = positive.state().active_coin_flip_frame().unwrap();
    let original = owner.return_result_occurrence.unwrap();
    let expected = owner.expected_return_result_process;
    assert_ne!(foreign_frame.process, expected);
    for fault in 0..7 {
        let mut hostile = positive.state().clone();
        match fault {
            0 => {
                hostile.return_result_frames.remove(&original);
                hostile
                    .return_result_frames
                    .insert(foreign_occurrence, foreign_frame.clone());
                hostile.next_return_result_occurrence_id = foreign_occurrence.0 + 1;
                hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                hostile
                    .active_coin_flip_frame_mut()
                    .unwrap()
                    .return_result_occurrence = Some(foreign_occurrence);
                assert_eq!(
                    hostile
                        .active_coin_flip_frame()
                        .unwrap()
                        .expected_return_result_process,
                    expected
                );
            }
            1 => {
                hostile.return_result_frames.clear();
            }
            2 => {
                hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .remove(&ReturnResultId(7));
            }
            3 => {
                hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .insert(
                        ReturnResultId(7),
                        InstructionResult::RevealedCards(RevealedInstructionResult {
                            revealed_cards: vec![],
                        }),
                    );
            }
            4 => {
                hostile
                    .active_coin_flip_frame_mut()
                    .unwrap()
                    .expected_return_result_process = None;
            }
            5 => {
                hostile
                    .active_coin_flip_frame_mut()
                    .unwrap()
                    .return_result_occurrence = None;
            }
            6 => {
                let InstructionResult::ZoneChanges(records) = hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .get_mut(&ReturnResultId(7))
                    .unwrap()
                else {
                    unreachable!()
                };
                records[0].trigger_source_context = None;
            }
            _ => unreachable!(),
        }
        let error = serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap())
            .unwrap_err()
            .to_string();
        if fault == 0 || fault == 4 {
            assert!(
                error.contains("instruction-result process owner mismatch"),
                "{error}"
            );
        }
        let mut reached = GameRunner::from_state(hostile);
        let before = serde_json::to_value(reached.state()).unwrap();
        let error = reached
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![1],
            })
            .unwrap_err()
            .to_string();
        if fault == 0 || fault == 4 {
            assert!(
                error.contains("instruction-result process owner mismatch"),
                "{error}"
            );
        }
        assert_eq!(
            serde_json::to_value(reached.state()).unwrap(),
            before,
            "keep cannot mutate or consume the hostile owner"
        );
    }
    for viewer in [P0, P1] {
        let projected = engine::game::visibility::filter_state_for_viewer(positive.state(), viewer);
        assert!(projected.return_result_frames.is_empty());
        assert!(projected.resolution_stack.is_empty());
    }
    let projected = engine::game::visibility::filter_state_for_unseated_viewer(positive.state());
    assert!(projected.return_result_frames.is_empty());
    assert!(projected.resolution_stack.is_empty());
    let saved = serde_json::to_value(positive.state()).unwrap();
    *positive.state_mut() =
        serde_json::from_value(saved).expect("intact retained coin process reloads");
    positive
        .act(GameAction::SelectCoinFlips {
            keep_indices: vec![1],
        })
        .unwrap();
    assert_eq!(positive.state().players[0].life, 25);
    assert_eq!(positive.state().players[1].life, 22);
    assert!(matches!(
        positive.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(positive.state().stack.is_empty());
    assert!(positive.state().resolution_stack.is_empty());
    assert!(positive.state().return_result_frames.is_empty());
}

#[test]
fn departure_coin_genuine_empty_result_survives_both_public_keeps() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, spell) = departure_imported_coin_runner_with_population(2, false, true);
    runner.cast(spell).resolve();
    for _ in 0..2 {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { .. }
        ));
        let owner = runner.state().active_coin_flip_frame().unwrap();
        let frame = &runner.state().return_result_frames[&owner.return_result_occurrence.unwrap()];
        assert_eq!(frame.process, owner.expected_return_result_process);
        assert!(
            matches!(&frame.results[&engine::types::ability::ReturnResultId(7)],
            engine::types::game_state::InstructionResult::ZoneChanges(records) if records.is_empty())
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() =
            serde_json::from_value(saved).expect("completed empty is present on re-pause");
        runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![1],
            })
            .unwrap();
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(runner.state().players[1].life, 20);
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

// Shared setup for supplementary canonical programs through the paid public stack.
fn departure_public_program_runner(
    foreign_seven: bool,
    empty: bool,
    players: u8,
    build: impl FnOnce(
        &mut engine::game::scenario::GameScenario,
    ) -> (
        engine::types::ability::AbilityDefinition,
        Vec<engine::types::identifiers::ObjectId>,
    ),
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    departure_public_program_runner_with_setup(foreign_seven, empty, players, build, |_, _| {})
}

fn departure_public_program_runner_with_setup(
    foreign_seven: bool,
    empty: bool,
    players: u8,
    build: impl FnOnce(
        &mut engine::game::scenario::GameScenario,
    ) -> (
        engine::types::ability::AbilityDefinition,
        Vec<engine::types::identifiers::ObjectId>,
    ),
    setup: impl FnOnce(&mut GameState, &[engine::types::identifiers::ObjectId]),
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{AbilityDefinition, TargetFilter};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let mut victims = Vec::new();
    if !empty {
        victims.push(
            scenario
                .add_creature(P0, "Centaur Courser", 3, 3)
                .with_mana_cost(ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Green],
                })
                .id(),
        );
        victims.push(
            scenario
                .add_creature(
                    P0,
                    if foreign_seven {
                        "Rumbling Baloth"
                    } else {
                        "Grizzly Bears"
                    },
                    if foreign_seven { 4 } else { 2 },
                    if foreign_seven { 4 } else { 2 },
                )
                .with_mana_cost(if foreign_seven {
                    ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                    }
                } else {
                    ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::Green],
                    }
                })
                .id(),
        );
        victims.push(
            scenario
                .add_creature(P1, "Grizzly Bears", 2, 2)
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .id(),
        );
    }
    let (program, subjects): (AbilityDefinition, _) = build(&mut scenario);
    for seat in 0..players {
        for _ in 0..8 {
            scenario.add_card_to_library_top(PlayerId(seat), "Library witness");
        }
    }
    let mut root = departure_exile_definition(7);
    let engine::types::ability::Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
        unreachable!()
    };
    *target = TargetFilter::Or {
        filters: victims
            .iter()
            .map(|id| TargetFilter::SpecificObject { id: *id })
            .collect(),
    };
    root.sub_ability = Some(Box::new(program));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental retained public program", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    if foreign_seven {
        runner.state_mut().next_return_result_occurrence_id = 19;
        runner.state_mut().next_return_result_process_id = 37;
    }
    setup(runner.state_mut(), &victims);
    runner.cast(spell).commit();
    for _ in 0..8 {
        if !matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ) || runner.state().objects[&spell].zone == Zone::Graveyard
        {
            break;
        }
        runner
            .act(engine::types::actions::GameAction::PassPriority)
            .unwrap();
    }
    if let Some(pending) = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
    {
        if let Some((occurrence, result)) = pending.pending_return_result_producer {
            assert!(!runner.state().return_result_frames[&occurrence]
                .results
                .contains_key(&result));
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                0,
                "the real root was paid before its unsettled producer parked"
            );
            return (runner, spell, subjects);
        }
    }
    let records: Vec<_> = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .filter(|record| victims.contains(&record.object_id))
        .collect();
    assert_eq!(records.len(), victims.len());
    assert_eq!(
        records
            .iter()
            .filter(|record| record.controller == P0)
            .map(|record| record.power.unwrap())
            .sum::<i32>(),
        if empty {
            0
        } else if foreign_seven {
            7
        } else {
            5
        }
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.controller == P1)
            .map(|record| record.power.unwrap())
            .sum::<i32>(),
        if empty { 0 } else { 2 }
    );
    for id in victims {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    (runner, spell, subjects)
}

fn departure_dual_reader() -> engine::types::ability::AbilityDefinition {
    let mut reader = departure_gain_definition(7, PlayerId(0));
    reader.sub_ability = Some(Box::new(departure_gain_definition(7, PlayerId(1))));
    reader
}

fn departure_branch_reader() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter,
    };
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![
                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                departure_dual_reader(),
            ],
            kind: ChooseOneOfKind::Ordinary,
        },
    )
}

fn departure_assert_program_complete(
    runner: &engine::game::scenario::GameRunner,
    spell: engine::types::identifiers::ObjectId,
    gain0: i32,
    gain1: i32,
) {
    assert_eq!(runner.state().players[0].life, 20 + gain0);
    assert_eq!(runner.state().players[1].life, 20 + gain1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

#[test]
fn departure_vote_ballots_restore_import_through_both_branch_repauses() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, VoteSubject, VoteTally,
        VoteVisibility, VoterScope,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |_| {
            (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Vote {
                        choices: vec!["ordinary".into(), "addressed".into()],
                        per_choice_effect: vec![
                            Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
                            Box::new(departure_branch_reader()),
                        ],
                        starting_with: ControllerRef::You,
                        voter_scope: VoterScope::AllPlayers,
                        tally_mode: VoteTally::PerVote,
                        subject: VoteSubject::Named,
                        visibility: VoteVisibility::Open,
                    },
                ),
                vec![],
            )
        });
        for seat in 0..2 {
            let WaitingFor::VoteChoice {
                player,
                options,
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(expected),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("real public voting prompt with independent owner");
            };
            assert_eq!(*player, PlayerId(seat));
            assert_eq!(options, &["ordinary", "addressed"]);
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                Some(*expected)
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() =
                serde_json::from_value(saved).expect("every voting response preserves the owner");
            runner
                .act(GameAction::ChooseOption {
                    choice: "addressed".into(),
                })
                .unwrap();
        }
        for ballot in 0..2 {
            let WaitingFor::ChooseOneOfBranch {
                player,
                branches,
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(expected),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("each ballot reaches the actual imported branch body");
            };
            assert_eq!(*player, PlayerId(0));
            assert_eq!(branches.len(), 2);
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                Some(*expected)
            );
            assert_eq!(
                runner.state().players[0].life,
                20 + if empty { 0 } else { 5 * ballot }
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("ballot template and nested prompt survive re-pause reload");
            runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 10 },
            if empty { 0 } else { 4 },
        );
    }
}

#[test]
fn departure_die_modifier_and_result_table_restore_before_public_ignore() {
    use engine::game::scenario::P0;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DieResultBranch, DieRollModifier, Effect, QuantityExpr,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |scenario| {
            scenario.add_real_card(
                P0,
                "Barbarian Class",
                Zone::Battlefield,
                super::support::shared_card_db().expect(
                    "pinned card data is required for the public class replacement fixture",
                ),
            );
            let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect else {
                unreachable!()
            };
            (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::RollDie {
                        count: QuantityExpr::Fixed { value: 1 },
                        sides: 1,
                        results: vec![DieResultBranch {
                            min: 1,
                            max: u8::MAX,
                            effect: Box::new(departure_dual_reader()),
                        }],
                        modifier: Some(DieRollModifier::Add { value: amount }),
                    },
                ),
                vec![],
            )
        });
        let WaitingFor::DieKeepChoice {
            player,
            results,
            ignorable_indices,
            ignore_count,
        } = &runner.state().waiting_for
        else {
            panic!("real class replacement produces the deterministic tied ignore choice");
        };
        assert_eq!(*player, P0);
        assert_eq!(results, &[1, 1]);
        assert_eq!(ignorable_indices, &[0, 1]);
        assert_eq!(*ignore_count, 1);
        let pending = runner.state().resolution_stack.active_die_roll().unwrap();
        assert_eq!(
            runner.state().return_result_frames[&pending.return_result_occurrence.unwrap()].process,
            pending.expected_return_result_process
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("die table and unresolved modifier retain the executing owner");
        let response = runner
            .act(GameAction::SelectDieRolls {
                ignore_indices: vec![1],
            })
            .unwrap();
        assert_eq!(response.events.iter().filter(|event| matches!(event, GameEvent::DieRolled { result: Some(value), .. } if *value == if empty { 1 } else { 6 })).count(), 1);
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

#[test]
fn departure_explore_ordering_retains_import_through_both_public_dig_choices() {
    use engine::game::scenario::P0;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter, TargetRef};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, spell, explorers) =
        departure_public_program_runner(false, false, 2, |scenario| {
            let first = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
            let second = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
            let mut explore = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ExploreAll {
                    filter: TargetFilter::Or {
                        filters: vec![
                            TargetFilter::SpecificObject { id: first },
                            TargetFilter::SpecificObject { id: second },
                        ],
                    },
                },
            );
            explore.sub_ability = Some(Box::new(departure_dual_reader()));
            (explore, vec![first, second])
        });
    let WaitingFor::ExploreChoice {
        player,
        choosable,
        return_result_occurrence: Some(occurrence),
        pending_effect,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("two real same-player explorers require public ordering");
    };
    assert_eq!(*player, P0);
    assert_eq!(choosable, &explorers);
    assert_eq!(
        runner.state().return_result_frames[occurrence].process,
        pending_effect.return_result_process
    );
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved).expect("ordering owner reloads");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(explorers[1])),
        })
        .unwrap();
    for index in 0..2 {
        let WaitingFor::DigChoice { player, cards, .. } = &runner.state().waiting_for else {
            panic!("explore {index} really paused on its nonland choice");
        };
        assert_eq!(*player, P0);
        assert_eq!(cards.len(), 1);
        let card = cards[0];
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("each nested choice retains the actual imported outer tail");
        runner
            .act(GameAction::SelectCards { cards: vec![card] })
            .unwrap();
    }
    for explorer in explorers {
        assert_eq!(
            runner.state().objects[&explorer]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1)
        );
    }
    departure_assert_program_complete(&runner, spell, 5, 2);
}

#[test]
fn departure_clash_opponent_choice_captures_exact_imported_tail() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, spell, _) = departure_public_program_runner(false, false, 3, |_| {
        let mut clash = AbilityDefinition::new(AbilityKind::Spell, Effect::Clash);
        clash.sub_ability = Some(Box::new(departure_dual_reader()));
        (clash, vec![])
    });
    let WaitingFor::ClashChooseOpponent {
        player,
        candidates,
        return_result_occurrence: Some(occurrence),
        ability,
    } = &runner.state().waiting_for
    else {
        panic!("three-player clash asks its actual opponent choice");
    };
    assert_eq!(*player, PlayerId(0));
    assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
    assert_eq!(
        runner.state().return_result_frames[occurrence].process,
        ability.return_result_process
    );
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved).expect("clash opponent owner reloads");
    runner
        .act(GameAction::ChooseClashOpponent {
            opponent: PlayerId(2),
        })
        .unwrap();
    for player in [PlayerId(0), PlayerId(2)] {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ClashCardPlacement { player: actual, .. } if actual == player)
        );
        assert_eq!(runner.state().players[0].life, 20);
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("clash's captured continuation survives all public placements");
        runner
            .act(GameAction::ChooseTopOrBottom { top: false })
            .unwrap();
    }
    departure_assert_program_complete(&runner, spell, 5, 2);
}

#[derive(Clone, Copy, Debug)]
enum DepartureRetainedFamily {
    Vote,
    Die,
    Explore,
    Clash,
    Branch,
    Piles,
    PilesAll,
    ZoneOpponent,
    ZoneEach,
    ZoneReciprocal,
    ScopedSearch,
    Unless,
    UnlessOneOf,
    UnlessDiscard,
    UnlessSacrifice,
    UnlessBounce,
    DiscardTail,
    SacrificeTail,
    CoinSingle,
    CoinUntil,
    MultiTarget,
    CategoryCounter,
    CategoryExile,
}

fn departure_retained_family_runner(
    family: DepartureRetainedFamily,
    foreign: bool,
    empty: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::game::scenario::P0;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, DieResultBranch, DieRollModifier, Effect,
        QuantityExpr, TargetFilter, VoteSubject, VoteTally, VoteVisibility, VoterScope,
    };
    departure_public_program_runner_with_setup(
        foreign,
        empty,
        if matches!(
            family,
            DepartureRetainedFamily::Clash
                | DepartureRetainedFamily::Piles
                | DepartureRetainedFamily::PilesAll
                | DepartureRetainedFamily::ZoneOpponent
                | DepartureRetainedFamily::ZoneEach
                | DepartureRetainedFamily::ZoneReciprocal
        ) {
            3
        } else {
            2
        },
        |scenario| match family {
            DepartureRetainedFamily::Vote => (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Vote {
                        choices: vec!["ordinary".into(), "addressed".into()],
                        per_choice_effect: vec![
                            Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
                            Box::new(departure_branch_reader()),
                        ],
                        starting_with: ControllerRef::You,
                        voter_scope: VoterScope::AllPlayers,
                        tally_mode: VoteTally::PerVote,
                        subject: VoteSubject::Named,
                        visibility: VoteVisibility::Open,
                    },
                ),
                vec![],
            ),
            DepartureRetainedFamily::Die => {
                scenario.add_real_card(
                    P0,
                    "Barbarian Class",
                    Zone::Battlefield,
                    super::support::shared_card_db()
                        .expect("pinned class replacement card is required"),
                );
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                (
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::RollDie {
                            count: QuantityExpr::Fixed { value: 1 },
                            sides: 1,
                            results: vec![DieResultBranch {
                                min: 1,
                                max: u8::MAX,
                                effect: Box::new(departure_dual_reader()),
                            }],
                            modifier: Some(DieRollModifier::Add { value: amount }),
                        },
                    ),
                    vec![],
                )
            }
            DepartureRetainedFamily::Explore => {
                let first = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
                let second = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
                let mut explore = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ExploreAll {
                        filter: TargetFilter::Or {
                            filters: vec![
                                TargetFilter::SpecificObject { id: first },
                                TargetFilter::SpecificObject { id: second },
                            ],
                        },
                    },
                );
                explore.sub_ability = Some(Box::new(departure_dual_reader()));
                (explore, vec![first, second])
            }
            DepartureRetainedFamily::Clash => {
                let mut clash = AbilityDefinition::new(AbilityKind::Spell, Effect::Clash);
                clash.sub_ability = Some(Box::new(departure_dual_reader()));
                (clash, vec![])
            }
            DepartureRetainedFamily::Branch => (departure_branch_reader(), vec![]),
            DepartureRetainedFamily::CategoryCounter | DepartureRetainedFamily::CategoryExile => {
                use engine::types::ability::{
                    AbilityCost, Chooser, ForEachCategoryAction, IterationCategory,
                    ReplacementDefinition, ReplacementMode,
                };
                use engine::types::mana::ManaColor;
                let exile = matches!(family, DepartureRetainedFamily::CategoryExile);
                let mut subjects = vec![];
                for (name, color) in [
                    ("Supplemental category library red A", ManaColor::Red),
                    ("Supplemental category library red B", ManaColor::Red),
                    ("Supplemental category library green A", ManaColor::Green),
                    ("Supplemental category library green B", ManaColor::Green),
                ] {
                    let id = if exile {
                        scenario
                            .add_spell_to_library_top(P0, name, false)
                            .with_color(vec![color])
                            .id()
                    } else {
                        scenario
                            .add_artifact_from_oracle(P0, name, "")
                            .with_color(vec![color])
                            .id()
                    };
                    subjects.push(id);
                    if exile {
                        let replacement = ReplacementDefinition::new(
                            engine::types::replacements::ReplacementEvent::Moved,
                        )
                        .valid_card(TargetFilter::SpecificObject { id })
                        .destination_zone(Zone::Exile)
                        .mode(ReplacementMode::MayCost {
                            cost: AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                            },
                            decline: Some(Box::new(AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::NoOp,
                            ))),
                        });
                        scenario
                            .add_artifact_from_oracle(
                                P0,
                                "Supplemental category move replacement",
                                "",
                            )
                            .with_replacement_definition(replacement);
                    }
                }
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let mut filter = engine::types::ability::TypedFilter::new(
                    engine::types::ability::TypeFilter::Artifact,
                );
                filter
                    .properties
                    .push(engine::types::ability::FilterProp::Cmc {
                        comparator: engine::types::ability::Comparator::LT,
                        value: QuantityExpr::Sum {
                            exprs: vec![amount.clone(), QuantityExpr::Fixed { value: 1 }],
                        },
                    });
                let mut category = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ForEachCategory {
                        category: IterationCategory::Color,
                        chooser: Chooser::Controller,
                        action: if exile {
                            ForEachCategoryAction::ExileFromPool {
                                zone: Zone::Library,
                                up_to: true,
                            }
                        } else {
                            ForEachCategoryAction::PutCounter {
                                target: TargetFilter::Typed(filter),
                                counter_type: CounterType::Plus1Plus1,
                                count: amount,
                            }
                        },
                    },
                );
                if exile {
                    category.sub_ability = Some(Box::new(departure_dual_reader()));
                    let mut reveal = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::RevealTop {
                            player: TargetFilter::Controller,
                            count: 4,
                        },
                    );
                    reveal.sub_ability = Some(Box::new(category));
                    (reveal, subjects)
                } else {
                    (category, subjects)
                }
            }
            DepartureRetainedFamily::ZoneReciprocal => {
                use engine::types::ability::{
                    CardSelectionMode, ReciprocalZoneChoiceRole, ZoneChoiceCandidateSource,
                    ZoneChoiceChooser, ZoneOwner,
                };
                let mv = if empty {
                    0
                } else if foreign {
                    7
                } else {
                    5
                };
                let mut subjects = vec![];
                for name in [
                    "Supplemental reciprocal consumer A",
                    "Supplemental reciprocal consumer B",
                ] {
                    subjects.push(
                        scenario
                            .add_artifact_from_oracle(P0, name, "")
                            .with_mana_cost(engine::types::mana::ManaCost::Cost {
                                generic: mv,
                                shards: vec![],
                            })
                            .id(),
                    );
                }
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let mut filter = engine::types::ability::TypedFilter::new(
                    engine::types::ability::TypeFilter::Artifact,
                );
                filter
                    .properties
                    .push(engine::types::ability::FilterProp::Cmc {
                        comparator: engine::types::ability::Comparator::EQ,
                        value: amount,
                    });
                let mut consumer = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseFromZone {
                        count: 1,
                        zone: Zone::Battlefield,
                        additional_zones: vec![],
                        zone_owner: ZoneOwner::Controller,
                        filter: Some(TargetFilter::Typed(filter)),
                        chooser: ZoneChoiceChooser::ImmediatePriorSelectedCardOwner {
                            player: None,
                        },
                        candidate_source: ZoneChoiceCandidateSource::Direct,
                        reciprocal_role: Some(ReciprocalZoneChoiceRole::Consume),
                        up_to: false,
                        constraint: None,
                        selection: CardSelectionMode::Chosen,
                    },
                );
                consumer.sub_ability = Some(Box::new(departure_dual_reader()));
                let mut empty_producer = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseFromZone {
                        count: 1,
                        zone: Zone::Graveyard,
                        additional_zones: vec![],
                        zone_owner: ZoneOwner::Opponent,
                        filter: None,
                        chooser: ZoneChoiceChooser::Controller,
                        candidate_source: ZoneChoiceCandidateSource::Direct,
                        reciprocal_role: Some(ReciprocalZoneChoiceRole::Produce),
                        up_to: false,
                        constraint: None,
                        selection: CardSelectionMode::Chosen,
                    },
                );
                empty_producer.sub_ability = Some(Box::new(consumer));
                (empty_producer, subjects)
            }
            DepartureRetainedFamily::MultiTarget => {
                let mv = if empty {
                    0
                } else if foreign {
                    7
                } else {
                    5
                };
                let mut subjects = vec![];
                for name in [
                    "Supplemental optional target A",
                    "Supplemental optional target B",
                ] {
                    subjects.push(
                        scenario
                            .add_artifact_from_oracle(PlayerId(1), name, "")
                            .with_mana_cost(engine::types::mana::ManaCost::Cost {
                                generic: mv,
                                shards: vec![],
                            })
                            .id(),
                    );
                }
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let mut filter = engine::types::ability::TypedFilter::new(
                    engine::types::ability::TypeFilter::Artifact,
                )
                .controller(ControllerRef::SpecificPlayer { id: PlayerId(1) });
                filter
                    .properties
                    .push(engine::types::ability::FilterProp::Cmc {
                        comparator: engine::types::ability::Comparator::EQ,
                        value: amount,
                    });
                let mut body = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::Typed(filter),
                        scope: engine::types::ability::EffectScope::Single,
                        state: engine::types::ability::TapStateChange::Tap,
                    },
                );
                body.optional = true;
                body.optional_for = Some(engine::types::ability::OpponentMayScope::AnyOpponent);
                body.sub_ability = Some(Box::new(departure_dual_reader()));
                (body, subjects)
            }
            DepartureRetainedFamily::CoinSingle | DepartureRetainedFamily::CoinUntil => {
                scenario.add_real_card(
                    P0,
                    "Krark's Thumb",
                    Zone::Battlefield,
                    super::support::shared_card_db()
                        .expect("pinned real replacement card is required"),
                );
                let effect = if matches!(family, DepartureRetainedFamily::CoinSingle) {
                    Effect::FlipCoins {
                        flips: engine::types::ability::CoinFlipRun::single(),
                        win_effect: Some(Box::new(departure_branch_reader())),
                        lose_effect: Some(Box::new(departure_branch_reader())),
                        flipper: TargetFilter::Controller,
                    }
                } else {
                    Effect::FlipCoins {
                        flips: engine::types::ability::CoinFlipRun::UntilLoss,
                        win_effect: Some(Box::new(departure_dual_reader())),
                        lose_effect: None,
                        flipper: TargetFilter::Controller,
                    }
                };
                (AbilityDefinition::new(AbilityKind::Spell, effect), vec![])
            }
            DepartureRetainedFamily::DiscardTail | DepartureRetainedFamily::SacrificeTail => {
                let mut subjects = vec![];
                let effect = if matches!(family, DepartureRetainedFamily::DiscardTail) {
                    for name in [
                        "Supplemental discard tail A",
                        "Supplemental discard tail B",
                        "Supplemental discard tail C",
                        "Supplemental discard tail D",
                    ] {
                        subjects.push(scenario.add_card_to_hand(PlayerId(1), name));
                    }
                    Effect::Discard {
                        count: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::SpecificPlayer { id: PlayerId(1) },
                        filter: None,
                        selection: engine::types::ability::CardSelectionMode::Chosen,
                        unless_filter: None,
                    }
                } else {
                    for name in [
                        "Supplemental sacrifice tail A",
                        "Supplemental sacrifice tail B",
                        "Supplemental sacrifice tail C",
                    ] {
                        subjects.push(
                            scenario
                                .add_artifact_from_oracle(PlayerId(1), name, "")
                                .id(),
                        );
                    }
                    Effect::Sacrifice {
                        target: TargetFilter::Typed(
                            engine::types::ability::TypedFilter::new(
                                engine::types::ability::TypeFilter::Artifact,
                            )
                            .controller(ControllerRef::SpecificPlayer { id: PlayerId(1) }),
                        ),
                        count: QuantityExpr::Fixed { value: 2 },
                        min_count: 0,
                    }
                };
                let mut body = AbilityDefinition::new(AbilityKind::Spell, effect);
                body.sub_ability = Some(Box::new(departure_dual_reader()));
                (body, subjects)
            }
            DepartureRetainedFamily::Unless
            | DepartureRetainedFamily::UnlessOneOf
            | DepartureRetainedFamily::UnlessDiscard
            | DepartureRetainedFamily::UnlessSacrifice
            | DepartureRetainedFamily::UnlessBounce => {
                use engine::types::ability::{
                    AbilityCost, CardSelectionMode, DiscardSelfScope, SacrificeCost,
                    UnlessPayModifier,
                };
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let mut typed = engine::types::ability::TypedFilter::default();
                typed
                    .properties
                    .push(engine::types::ability::FilterProp::Cmc {
                        comparator: engine::types::ability::Comparator::LT,
                        value: QuantityExpr::Sum {
                            exprs: vec![amount.clone(), QuantityExpr::Fixed { value: 1 }],
                        },
                    });
                let filter = TargetFilter::Typed(typed);
                let mut subjects = vec![];
                let cost = match family {
                    DepartureRetainedFamily::Unless => AbilityCost::PayLife {
                        amount: amount.clone(),
                    },
                    DepartureRetainedFamily::UnlessOneOf => AbilityCost::OneOf {
                        costs: vec![
                            AbilityCost::PayLife {
                                amount: amount.clone(),
                            },
                            AbilityCost::ManaDynamic { quantity: amount },
                        ],
                    },
                    DepartureRetainedFamily::UnlessDiscard => {
                        subjects.push(
                            scenario.add_card_to_hand(PlayerId(1), "Supplemental discard member A"),
                        );
                        subjects.push(
                            scenario.add_card_to_hand(PlayerId(1), "Supplemental discard member B"),
                        );
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 2 },
                            filter: Some(filter),
                            selection: CardSelectionMode::Chosen,
                            self_scope: DiscardSelfScope::FromHand,
                        }
                    }
                    DepartureRetainedFamily::UnlessSacrifice
                    | DepartureRetainedFamily::UnlessBounce => {
                        subjects.push(
                            scenario
                                .add_artifact_from_oracle(
                                    PlayerId(1),
                                    "Supplemental payment member A",
                                    "",
                                )
                                .id(),
                        );
                        subjects.push(
                            scenario
                                .add_artifact_from_oracle(
                                    PlayerId(1),
                                    "Supplemental payment member B",
                                    "",
                                )
                                .id(),
                        );
                        if matches!(family, DepartureRetainedFamily::UnlessSacrifice) {
                            AbilityCost::Sacrifice(SacrificeCost::count(filter, 2))
                        } else {
                            AbilityCost::ReturnToHand {
                                count: 2,
                                filter: Some(filter),
                                from_zone: None,
                            }
                        }
                    }
                    _ => unreachable!(),
                };
                let mut body = departure_dual_reader();
                body.unless_pay = Some(UnlessPayModifier {
                    cost,
                    payer: TargetFilter::SpecificPlayer { id: PlayerId(1) },
                });
                (body, subjects)
            }
            DepartureRetainedFamily::ScopedSearch => {
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let mut filter = engine::types::ability::TypedFilter::default();
                filter
                    .properties
                    .push(engine::types::ability::FilterProp::Cmc {
                        comparator: engine::types::ability::Comparator::LT,
                        value: QuantityExpr::Sum {
                            exprs: vec![amount, QuantityExpr::Fixed { value: 1 }],
                        },
                    });
                let mut search = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SearchLibrary {
                        source_zones: vec![Zone::Library],
                        filter: TargetFilter::Typed(filter),
                        count: QuantityExpr::Fixed { value: 1 },
                        reveal: true,
                        target_player: None,
                        selection_constraint:
                            engine::types::ability::SearchSelectionConstraint::None,
                        split: None,
                    },
                );
                search.player_scope = Some(engine::types::ability::PlayerFilter::All);
                let delivery: Effect = serde_json::from_value(serde_json::json!({ "type": "ChangeZone", "origin": "Library", "destination": "Hand", "target": { "type": "ParentTarget" } })).unwrap();
                search.sub_ability = Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    delivery,
                )));
                (search, vec![])
            }
            DepartureRetainedFamily::ZoneOpponent | DepartureRetainedFamily::ZoneEach => {
                let mv = if empty {
                    0
                } else if foreign {
                    7
                } else {
                    5
                };
                let each = matches!(family, DepartureRetainedFamily::ZoneEach);
                let mut subjects = vec![];
                for owner in if each {
                    vec![PlayerId(1), PlayerId(2)]
                } else {
                    vec![P0]
                } {
                    for name in [
                        "Supplemental canonical zone-pool member A",
                        "Supplemental canonical zone-pool member B",
                    ] {
                        subjects.push(
                            scenario
                                .add_artifact_from_oracle(owner, name, "")
                                .with_mana_cost(engine::types::mana::ManaCost::Cost {
                                    generic: mv,
                                    shards: vec![],
                                })
                                .id(),
                        );
                    }
                }
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let mut typed = engine::types::ability::TypedFilter::new(
                    engine::types::ability::TypeFilter::Artifact,
                );
                typed
                    .properties
                    .push(engine::types::ability::FilterProp::Cmc {
                        comparator: engine::types::ability::Comparator::EQ,
                        value: amount,
                    });
                let mut choose = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseFromZone {
                        count: 1,
                        zone: Zone::Battlefield,
                        additional_zones: vec![],
                        zone_owner: if each {
                            engine::types::ability::ZoneOwner::Each(
                                engine::types::ability::PerPlayerScope::OtherPlayers,
                            )
                        } else {
                            engine::types::ability::ZoneOwner::Controller
                        },
                        filter: Some(TargetFilter::Typed(typed)),
                        chooser: if each {
                            engine::types::ability::ZoneChoiceChooser::Controller
                        } else {
                            engine::types::ability::ZoneChoiceChooser::Opponent
                        },
                        candidate_source: engine::types::ability::ZoneChoiceCandidateSource::Direct,
                        reciprocal_role: None,
                        up_to: false,
                        constraint: None,
                        selection: engine::types::ability::CardSelectionMode::Chosen,
                    },
                );
                choose.sub_ability = Some(Box::new(departure_dual_reader()));
                (choose, subjects)
            }
            DepartureRetainedFamily::Piles | DepartureRetainedFamily::PilesAll => {
                let all = matches!(family, DepartureRetainedFamily::PilesAll);
                if all {
                    for seat in [PlayerId(1), PlayerId(2)] {
                        scenario.add_artifact_from_oracle(seat, "Supplemental pile member A", "");
                        scenario.add_artifact_from_oracle(seat, "Supplemental pile member B", "");
                    }
                }
                (
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::SeparateIntoPiles {
                            partition_subject: if all {
                                VoterScope::EachOpponent
                            } else {
                                VoterScope::AnOpponent
                            },
                            object_filter: TargetFilter::Typed(
                                engine::types::ability::TypedFilter::new(
                                    engine::types::ability::TypeFilter::Artifact,
                                ),
                            ),
                            chooser: engine::types::ability::PlayerScope::Controller,
                            chosen_pile_effect: Box::new(departure_gain_definition(7, PlayerId(0))),
                            unchosen_pile_effect: Some(Box::new(departure_gain_definition(
                                7,
                                PlayerId(1),
                            ))),
                            pile_source: if all {
                                engine::types::ability::PileSource::Battlefield
                            } else {
                                engine::types::ability::PileSource::RevealedFromLibraryTop {
                                    count: 2,
                                }
                            },
                        },
                    ),
                    vec![],
                )
            }
        },
        |state, _| {
            if matches!(family, DepartureRetainedFamily::CategoryExile) {
                let cards: Vec<_> = state
                    .objects
                    .values()
                    .filter(|object| object.name.starts_with("Supplemental category library"))
                    .map(|object| object.id)
                    .collect();
                assert_eq!(cards.len(), 4);
                state.players[0].library.retain(|id| !cards.contains(id));
                for id in cards.iter().rev() {
                    state.players[0].library.push_front(*id);
                }
            }
        },
    )
}

fn departure_retained_public_action(state: &GameState) -> engine::types::actions::GameAction {
    use engine::types::ability::TargetRef;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    match &state.waiting_for {
        WaitingFor::VoteChoice { options, .. } => {
            assert_eq!(options, &["ordinary", "addressed"]);
            GameAction::ChooseOption {
                choice: "addressed".into(),
            }
        }
        WaitingFor::ChooseOneOfBranch { branches, .. } => {
            assert_eq!(branches.len(), 2);
            GameAction::ChooseBranch { index: 1 }
        }
        WaitingFor::DieKeepChoice {
            results,
            ignorable_indices,
            ignore_count,
            ..
        } => {
            assert_eq!(results, &[1, 1]);
            assert_eq!(ignorable_indices, &[0, 1]);
            assert_eq!(*ignore_count, 1);
            GameAction::SelectDieRolls {
                ignore_indices: vec![1],
            }
        }
        WaitingFor::ExploreChoice { choosable, .. } => {
            assert_eq!(choosable.len(), 2);
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(choosable[1])),
            }
        }
        WaitingFor::DigChoice { cards, .. } => {
            assert_eq!(cards.len(), 1);
            GameAction::SelectCards {
                cards: vec![cards[0]],
            }
        }
        WaitingFor::ClashChooseOpponent { candidates, .. } => {
            assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
            GameAction::ChooseClashOpponent {
                opponent: PlayerId(2),
            }
        }
        WaitingFor::ClashCardPlacement { .. } => GameAction::ChooseTopOrBottom { top: false },
        WaitingFor::OpponentMayChoice { player, .. } => {
            assert_eq!(*player, PlayerId(1));
            GameAction::DecideOptionalEffect { accept: true }
        }
        WaitingFor::MultiTargetSelection {
            player,
            legal_targets,
            min_targets,
            max_targets,
            ..
        } => {
            assert_eq!(*player, PlayerId(1));
            assert_eq!((*min_targets, *max_targets), (1, 1));
            assert_eq!(legal_targets.len(), 2);
            GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Object(legal_targets[1])],
            }
        }
        WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 1 },
        WaitingFor::CoinFlipKeepChoice {
            player,
            results,
            keep_count,
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(results.len(), 2);
            assert_eq!(*keep_count, 1);
            GameAction::SelectCoinFlips {
                keep_indices: vec![1],
            }
        }
        WaitingFor::DiscardChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(*player, PlayerId(1));
            assert_eq!(*count, 2);
            assert_eq!(cards.len(), 4);
            GameAction::SelectCards {
                cards: cards[cards.len() - *count..].to_vec(),
            }
        }
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            effect_kind,
            ..
        } => {
            assert_eq!(*player, PlayerId(1));
            assert_eq!(*count, 2);
            assert_eq!(cards.len(), 3);
            assert_eq!(*effect_kind, engine::types::ability::EffectKind::Sacrifice);
            GameAction::SelectCards {
                cards: cards[cards.len() - *count..].to_vec(),
            }
        }
        WaitingFor::UnlessPayment { player, .. } => {
            assert_eq!(*player, PlayerId(1));
            GameAction::PayUnlessCost { pay: false }
        }
        WaitingFor::UnlessPaymentChooseCost { player, costs, .. } => {
            assert_eq!(*player, PlayerId(1));
            assert_eq!(costs.len(), 2);
            GameAction::ChooseUnlessCostBranch {
                choice: engine::types::actions::UnlessCostBranch::Decline,
            }
        }
        WaitingFor::WardDiscardChoice { player, cards, .. } => {
            assert_eq!(*player, PlayerId(1));
            GameAction::SelectCards {
                cards: vec![*cards.last().unwrap()],
            }
        }
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        }
        | WaitingFor::UnlessBounceChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, PlayerId(1));
            GameAction::SelectCards {
                cards: vec![*permanents.last().unwrap()],
            }
        }
        WaitingFor::SearchChoice { cards, count, .. } => {
            assert!(
                state.pending_scoped_library_search.is_some(),
                "the actual simultaneous protocol owns this choice"
            );
            assert_eq!(*count, 1);
            assert!(!cards.is_empty());
            GameAction::SelectCards {
                cards: vec![*cards.last().unwrap()],
            }
        }
        WaitingFor::ChooseFromZoneOpponentChooser {
            candidates,
            purpose,
            ..
        } => {
            assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
            assert!(matches!(
                purpose,
                engine::types::game_state::ZoneOpponentChooserPurpose::Ordinary
                    | engine::types::game_state::ZoneOpponentChooserPurpose::PerPlayerChoiceOrder
                    | engine::types::game_state::ZoneOpponentChooserPurpose::BindReciprocalConsume
                    | engine::types::game_state::ZoneOpponentChooserPurpose::SubstituteChooser
            ));
            GameAction::ChooseZoneOpponentChooser {
                opponent: PlayerId(2),
            }
        }
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(
                *player,
                if let Some(frame) = state.active_per_player_zone_choice() {
                    frame.nominee.unwrap_or(frame.ability.controller)
                } else if let Some(frame) = state.active_per_category_zone_choice() {
                    frame.ability.controller
                } else {
                    PlayerId(2)
                }
            );
            assert_eq!(cards.len(), 2);
            assert_eq!(*count, 1);
            GameAction::SelectCards {
                cards: vec![cards[1]],
            }
        }
        WaitingFor::SeparatePilesChooseOpponent {
            candidates,
            eligible,
            ..
        } => {
            assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
            assert_eq!(eligible.len(), 2);
            GameAction::ChoosePileOpponent {
                opponent: PlayerId(2),
            }
        }
        WaitingFor::SeparatePilesPartition { eligible, .. } => {
            assert_eq!(eligible.len(), 2);
            GameAction::SubmitPilePartition {
                pile_a: vec![eligible[0]],
            }
        }
        WaitingFor::SeparatePilesChoice { current, .. } => {
            assert_eq!(current.pile_a.len(), 1);
            assert_eq!(current.pile_b.len(), 1);
            GameAction::ChoosePile {
                pile: engine::types::game_state::PileSide::B,
            }
        }
        other => panic!("unexpected retained-program public prompt: {other:?}"),
    }
}

fn departure_drain_retained_public_program(runner: &mut engine::game::scenario::GameRunner) {
    for step in 0..24 {
        if matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ) {
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            return;
        }
        departure_assert_private_result_views(runner.state());
        let action = departure_retained_public_action(runner.state());
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("every reached retained owner and child survives reload");
        runner.act(action).unwrap();
        assert!(step < 23, "retained program must terminate");
    }
}

fn departure_rewrite_lookup(
    value: &mut serde_json::Value,
    original: &serde_json::Value,
    replacement: &serde_json::Value,
) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(lookup) = fields.get_mut("return_result_occurrence") {
                if lookup == original {
                    *lookup = replacement.clone();
                }
            }
            for (key, child) in fields {
                if key != "return_result_frames" {
                    departure_rewrite_lookup(child, original, replacement);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                departure_rewrite_lookup(child, original, replacement);
            }
        }
        _ => {}
    }
}

// Actual public prompts plus supplemental typed stored custody use the same redactor.
fn departure_assert_private_result_views(state: &GameState) {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::types::game_state::PendingTriggerOrder;
    fn assert_private_owners(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, child) in fields {
                    match key.as_str() {
                        "return_result_process"
                        | "expected_return_result_process"
                        | "return_result_occurrence"
                        | "active_return_result_occurrence" => {
                            assert!(child.is_null(), "private result owner {key}: {child}");
                        }
                        "next_return_result_occurrence_id" | "next_return_result_process_id" => {
                            assert_eq!(child, &serde_json::json!(1), "private allocator {key}");
                        }
                        _ => assert_private_owners(child),
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for child in values {
                    assert_private_owners(child);
                }
            }
            _ => {}
        }
    }
    let mut stored = state.clone();
    stored.pending_trigger_order = Some(PendingTriggerOrder {
        groups: vec![],
        resume_after_ordering: Some(Box::new(state.waiting_for.clone())),
    });
    for authoritative in [state, &stored] {
        for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
            let projected = match viewer {
                Some(player) => {
                    engine::game::visibility::filter_state_for_viewer(authoritative, player)
                }
                None => engine::game::visibility::filter_state_for_unseated_viewer(authoritative),
            };
            assert!(projected.return_result_frames.is_empty());
            assert!(projected.resolution_stack.is_empty());
            let plain = serde_json::to_value(&projected).unwrap();
            let wire =
                serde_json::to_value(&ClientGameStateRef::wrap(authoritative, viewer)).unwrap();
            for value in [plain, wire] {
                assert_private_owners(&value);
            }
        }
    }
}

fn departure_assert_retained_hostile_reads(
    positive: &engine::game::scenario::GameRunner,
    family: DepartureRetainedFamily,
    foreign_occurrence: engine::types::game_state::ReturnResultOccurrenceId,
    foreign_frame: engine::types::game_state::ReturnResultFrame,
) {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::ReturnResultId;
    use engine::types::game_state::{InstructionResult, RevealedInstructionResult, WaitingFor};
    let (&original, original_frame) = positive.state().return_result_frames.iter().next().unwrap();
    assert_ne!(foreign_frame.process, original_frame.process);
    let action = departure_retained_public_action(positive.state());
    for fault in 0..7 {
        let mut hostile = positive.state().clone();
        match fault {
            0 => {
                hostile.return_result_frames.remove(&original);
                hostile
                    .return_result_frames
                    .insert(foreign_occurrence, foreign_frame.clone());
                hostile.next_return_result_occurrence_id = foreign_occurrence.0 + 1;
                hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                let from = serde_json::to_value(original).unwrap();
                let to = serde_json::to_value(foreign_occurrence).unwrap();
                let mut waiting = serde_json::to_value(&hostile.waiting_for).unwrap();
                departure_rewrite_lookup(&mut waiting, &from, &to);
                hostile.waiting_for = serde_json::from_value(waiting)
                    .expect("lookup-only mutation preserves the existing prompt payload");
                let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                departure_rewrite_lookup(&mut stack, &from, &to);
                hostile.resolution_stack = serde_json::from_value(stack).expect("all existing work lookup slots change together, independently of every saved P");
                if let Some(pending) = hostile.pending_scoped_library_search.as_mut() {
                    assert_eq!(pending.return_result_occurrence, Some(original));
                    pending.return_result_occurrence = Some(foreign_occurrence);
                }
                assert!(!hostile.return_result_frames.contains_key(&original));
            }
            1 => {
                hostile.return_result_frames.clear();
            }
            2 => {
                hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .remove(&ReturnResultId(7));
            }
            3 => {
                hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .insert(
                        ReturnResultId(7),
                        InstructionResult::RevealedCards(RevealedInstructionResult {
                            revealed_cards: vec![],
                        }),
                    );
            }
            4 => {
                match &mut hostile.waiting_for {
                    WaitingFor::VoteChoice {
                        expected_return_result_process,
                        ..
                    }
                    | WaitingFor::ChooseOneOfBranch {
                        expected_return_result_process,
                        ..
                    }
                    | WaitingFor::SeparatePilesChooseOpponent {
                        expected_return_result_process,
                        ..
                    }
                    | WaitingFor::SeparatePilesPartition {
                        expected_return_result_process,
                        ..
                    }
                    | WaitingFor::SeparatePilesChoice {
                        expected_return_result_process,
                        ..
                    } => *expected_return_result_process = None,
                    WaitingFor::UnlessPayment { pending_effect, .. }
                    | WaitingFor::UnlessPaymentChooseCost { pending_effect, .. }
                    | WaitingFor::WardDiscardChoice { pending_effect, .. }
                    | WaitingFor::WardSacrificeChoice { pending_effect, .. }
                    | WaitingFor::UnlessBounceChoice { pending_effect, .. }
                    | WaitingFor::ExploreChoice { pending_effect, .. } => {
                        pending_effect.return_result_process = None
                    }
                    WaitingFor::ClashChooseOpponent { ability, .. }
                    | WaitingFor::ChooseFromZoneOpponentChooser { ability, .. } => {
                        ability.return_result_process = None
                    }
                    WaitingFor::ChooseFromZoneChoice { .. } => {
                        if hostile.active_per_player_zone_choice().is_some() {
                            let mut pending = hostile
                                .take_active_per_player_zone_choice()
                                .unwrap()
                                .unwrap();
                            pending.ability.return_result_process = None;
                            hostile.push_per_player_zone_choice(pending);
                        } else if hostile.active_per_category_zone_choice().is_some() {
                            let mut pending = hostile
                                .take_active_per_category_zone_choice()
                                .unwrap()
                                .unwrap();
                            pending.ability.return_result_process = None;
                            hostile.push_per_category_zone_choice(pending);
                        } else {
                            let mut stack =
                                serde_json::to_value(&hostile.resolution_stack).unwrap();
                            assert!(departure_clear_continuation_process(&mut stack) > 0);
                            hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                        }
                    }
                    WaitingFor::DiscardChoice { .. } | WaitingFor::EffectZoneChoice { .. } => {
                        let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                        assert!(departure_clear_continuation_process(&mut stack) > 0);
                        hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                    }
                    WaitingFor::OpponentMayChoice { .. } => {
                        hostile
                            .active_optional_effect_frame_mut()
                            .unwrap()
                            .ability
                            .return_result_process = None
                    }
                    WaitingFor::MultiTargetSelection {
                        pending_ability, ..
                    } => pending_ability.return_result_process = None,
                    WaitingFor::ReplacementChoice { .. } => {
                        let engine::types::game_state::BatchCompletion::ForEachCategoryExileComplete { ability, .. } = hostile.active_batch_delivery_mut().unwrap().completion.as_mut().unwrap() else { unreachable!() };
                        ability.return_result_process = None;
                    }
                    WaitingFor::CoinFlipKeepChoice { .. } => {
                        hostile
                            .active_coin_flip_frame_mut()
                            .unwrap()
                            .expected_return_result_process = None
                    }
                    WaitingFor::SearchChoice { .. } => {
                        hostile
                            .pending_scoped_library_search
                            .as_mut()
                            .unwrap()
                            .ability
                            .return_result_process = None
                    }
                    WaitingFor::DieKeepChoice { .. } => {
                        let mut pending =
                            hostile.resolution_stack.active_die_roll().unwrap().clone();
                        pending.expected_return_result_process = None;
                        hostile.replace_active_die_roll_frame(pending).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            5 => {
                match &mut hostile.waiting_for {
                    WaitingFor::UnlessPayment {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::UnlessPaymentChooseCost {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::WardDiscardChoice {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::WardSacrificeChoice {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::UnlessBounceChoice {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::VoteChoice {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::ChooseOneOfBranch {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::ExploreChoice {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::ClashChooseOpponent {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::ChooseFromZoneOpponentChooser {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::SeparatePilesChooseOpponent {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::SeparatePilesPartition {
                        return_result_occurrence,
                        ..
                    }
                    | WaitingFor::SeparatePilesChoice {
                        return_result_occurrence,
                        ..
                    } => *return_result_occurrence = None,
                    WaitingFor::ChooseFromZoneChoice { .. } => {
                        if hostile.active_per_player_zone_choice().is_some() {
                            let mut pending = hostile
                                .take_active_per_player_zone_choice()
                                .unwrap()
                                .unwrap();
                            pending.return_result_occurrence = None;
                            hostile.push_per_player_zone_choice(pending);
                        } else if hostile.active_per_category_zone_choice().is_some() {
                            let mut pending = hostile
                                .take_active_per_category_zone_choice()
                                .unwrap()
                                .unwrap();
                            pending.return_result_occurrence = None;
                            hostile.push_per_category_zone_choice(pending);
                        } else {
                            let mut stack =
                                serde_json::to_value(&hostile.resolution_stack).unwrap();
                            departure_rewrite_lookup(
                                &mut stack,
                                &serde_json::to_value(original).unwrap(),
                                &serde_json::Value::Null,
                            );
                            hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                        }
                    }
                    WaitingFor::DiscardChoice { .. } | WaitingFor::EffectZoneChoice { .. } => {
                        let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                        departure_rewrite_lookup(
                            &mut stack,
                            &serde_json::to_value(original).unwrap(),
                            &serde_json::Value::Null,
                        );
                        hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                    }
                    WaitingFor::OpponentMayChoice { .. } => {
                        hostile
                            .active_optional_effect_frame_mut()
                            .unwrap()
                            .return_result_occurrence = None
                    }
                    WaitingFor::MultiTargetSelection {
                        return_result_occurrence,
                        ..
                    } => *return_result_occurrence = None,
                    WaitingFor::ReplacementChoice { .. } => {
                        let engine::types::game_state::BatchCompletion::ForEachCategoryExileComplete { return_result_occurrence, .. } = hostile.active_batch_delivery_mut().unwrap().completion.as_mut().unwrap() else { unreachable!() };
                        *return_result_occurrence = None;
                    }
                    WaitingFor::CoinFlipKeepChoice { .. } => {
                        hostile
                            .active_coin_flip_frame_mut()
                            .unwrap()
                            .return_result_occurrence = None
                    }
                    WaitingFor::SearchChoice { .. } => {
                        hostile
                            .pending_scoped_library_search
                            .as_mut()
                            .unwrap()
                            .return_result_occurrence = None
                    }
                    WaitingFor::DieKeepChoice { .. } => {
                        let mut pending =
                            hostile.resolution_stack.active_die_roll().unwrap().clone();
                        pending.return_result_occurrence = None;
                        hostile.replace_active_die_roll_frame(pending).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            6 => {
                let InstructionResult::ZoneChanges(records) = hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .get_mut(&ReturnResultId(7))
                    .unwrap()
                else {
                    unreachable!()
                };
                records[0].trigger_source_context = None;
            }
            _ => unreachable!(),
        }
        let saved = serde_json::to_value(&hostile).unwrap();
        let error = serde_json::from_value::<GameState>(saved)
            .unwrap_err()
            .to_string();
        if fault == 0 || fault == 4 {
            assert!(
                error.contains("instruction-result process owner mismatch"),
                "{family:?} reload: {error}"
            );
        }
        let mut reached = GameRunner::from_state(hostile);
        let before = serde_json::to_value(reached.state()).unwrap();
        let error = reached.act(action.clone()).unwrap_err().to_string();
        if fault == 0 || fault == 4 {
            assert!(
                error.contains("instruction-result process owner mismatch"),
                "{family:?} public action: {error}"
            );
        }
        assert_eq!(
            serde_json::to_value(reached.state()).unwrap(),
            before,
            "{family:?} invalid response cannot mutate or consume the existing work"
        );
    }
    departure_assert_private_result_views(positive.state());
}

#[test]
fn departure_retained_branch_vote_die_explore_clash_reject_foreign_before_public_mutation() {
    for family in [
        DepartureRetainedFamily::Branch,
        DepartureRetainedFamily::Vote,
        DepartureRetainedFamily::Die,
        DepartureRetainedFamily::Explore,
        DepartureRetainedFamily::Clash,
        DepartureRetainedFamily::Piles,
        DepartureRetainedFamily::PilesAll,
        DepartureRetainedFamily::ZoneOpponent,
        DepartureRetainedFamily::ZoneEach,
        DepartureRetainedFamily::ZoneReciprocal,
        DepartureRetainedFamily::ScopedSearch,
        DepartureRetainedFamily::Unless,
        DepartureRetainedFamily::UnlessOneOf,
        DepartureRetainedFamily::UnlessDiscard,
        DepartureRetainedFamily::UnlessSacrifice,
        DepartureRetainedFamily::UnlessBounce,
        DepartureRetainedFamily::CategoryCounter,
        DepartureRetainedFamily::CategoryExile,
        DepartureRetainedFamily::MultiTarget,
        DepartureRetainedFamily::CoinSingle,
        DepartureRetainedFamily::DiscardTail,
        DepartureRetainedFamily::SacrificeTail,
    ] {
        let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
        assert_eq!(
            foreign.state().return_result_frames.len(),
            1,
            "{family:?} genuinely reached its foreign-seven owner"
        );
        let (&foreign_occurrence, foreign_frame) =
            foreign.state().return_result_frames.iter().next().unwrap();
        let foreign_frame = foreign_frame.clone();
        departure_drain_retained_public_program(&mut foreign);
        let multiplier = if matches!(
            family,
            DepartureRetainedFamily::ScopedSearch | DepartureRetainedFamily::CategoryCounter
        ) {
            0
        } else if matches!(
            family,
            DepartureRetainedFamily::Vote | DepartureRetainedFamily::PilesAll
        ) {
            2
        } else {
            1
        };
        departure_assert_program_complete(&foreign, foreign_spell, 7 * multiplier, 2 * multiplier);
        let (positive, spell, _) = departure_retained_family_runner(family, false, false);
        assert_eq!(positive.state().return_result_frames.len(), 1);
        departure_assert_retained_hostile_reads(
            &positive,
            family,
            foreign_occurrence,
            foreign_frame.clone(),
        );
        let mut intact = positive;
        departure_drain_retained_public_program(&mut intact);
        departure_assert_program_complete(&intact, spell, 5 * multiplier, 2 * multiplier);
        let (mut empty, empty_spell, _) = departure_retained_family_runner(family, false, true);
        departure_drain_retained_public_program(&mut empty);
        departure_assert_program_complete(&empty, empty_spell, 0, 0);
    }
}

#[test]
fn departure_initial_entry_uses_saved_process_when_attribution_root_has_another_process() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut foreign, foreign_spell, [foreign_first, foreign_second]) =
        departure_zone_entry_prompt(true, false, false, false);
    let WaitingFor::EffectZoneChoice {
        return_result_process: Some(foreign_process),
        ..
    } = foreign.state().waiting_for
    else {
        unreachable!()
    };
    foreign
        .act(GameAction::SelectCards {
            cards: vec![foreign_second],
        })
        .unwrap();
    departure_assert_entry_complete(&foreign, foreign_spell, foreign_second, foreign_first, 7);
    for (predicate, gate) in [(false, false), (true, false), (false, true)] {
        let (mut runner, spell, [first, second]) =
            departure_zone_entry_prompt(false, false, predicate, gate);
        let WaitingFor::EffectZoneChoice {
            return_result_process: Some(expected),
            ..
        } = runner.state().waiting_for
        else {
            unreachable!()
        };
        assert_ne!(expected, foreign_process);
        runner.state_mut().next_return_result_process_id = foreign_process.0 + 1;
        runner
            .state_mut()
            .resolving_stack_entry
            .as_mut()
            .expect("existing attribution root")
            .ability_mut()
            .expect("spell's existing ability view")
            .return_result_process = Some(foreign_process);
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("attribution's process is not the prompt's independent authority");
        let WaitingFor::EffectZoneChoice {
            return_result_process,
            ..
        } = runner.state().waiting_for
        else {
            unreachable!()
        };
        assert_eq!(return_result_process, Some(expected));
        runner
            .act(GameAction::SelectCards {
                cards: vec![second],
            })
            .unwrap();
        departure_assert_entry_complete(&runner, spell, second, first, 5);
        assert_eq!(runner.state().objects[&second].tapped, gate);
    }
}

#[test]
fn departure_pile_election_partition_and_each_subject_choice_keep_exact_process() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{PileSide, WaitingFor};
    let (mut foreign, foreign_spell, _) =
        departure_retained_family_runner(DepartureRetainedFamily::Piles, true, false);
    let (&foreign_occurrence, foreign_frame) =
        foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    for all in [false, true] {
        let (mut runner, spell, _) = departure_retained_family_runner(
            if all {
                DepartureRetainedFamily::PilesAll
            } else {
                DepartureRetainedFamily::Piles
            },
            false,
            false,
        );
        if !all {
            let WaitingFor::SeparatePilesChooseOpponent {
                player,
                candidates,
                eligible,
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(expected),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("real three-player partitioner election");
            };
            assert_eq!(*player, PlayerId(0));
            assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
            assert_eq!(eligible.len(), 2);
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                Some(*expected)
            );
            departure_assert_retained_hostile_reads(
                &runner,
                if all {
                    DepartureRetainedFamily::PilesAll
                } else {
                    DepartureRetainedFamily::Piles
                },
                foreign_occurrence,
                foreign_frame.clone(),
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("partitioner election retains the independent process");
            runner
                .act(GameAction::ChoosePileOpponent {
                    opponent: PlayerId(2),
                })
                .unwrap();
        }
        let subject_count = if all { 2 } else { 1 };
        for index in 0..subject_count {
            let WaitingFor::SeparatePilesPartition {
                player,
                eligible,
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(expected),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("each actual subject partitions");
            };
            assert_eq!(
                *player,
                if all {
                    PlayerId(index + 1)
                } else {
                    PlayerId(2)
                }
            );
            assert_eq!(eligible.len(), 2);
            let first = eligible[0];
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                Some(*expected)
            );
            departure_assert_retained_hostile_reads(
                &runner,
                if all {
                    DepartureRetainedFamily::PilesAll
                } else {
                    DepartureRetainedFamily::Piles
                },
                foreign_occurrence,
                foreign_frame.clone(),
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() =
                serde_json::from_value(saved).expect("every partition re-pause reloads");
            runner
                .act(GameAction::SubmitPilePartition {
                    pile_a: vec![first],
                })
                .unwrap();
            assert_eq!(runner.state().players[0].life, 20);
            assert_eq!(runner.state().players[1].life, 20);
        }
        for index in 0..subject_count {
            let WaitingFor::SeparatePilesChoice {
                player,
                current,
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(expected),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("real chooser completes each subject");
            };
            assert_eq!(*player, PlayerId(0));
            assert_eq!(current.pile_a.len(), 1);
            assert_eq!(current.pile_b.len(), 1);
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                Some(*expected)
            );
            departure_assert_retained_hostile_reads(
                &runner,
                if all {
                    DepartureRetainedFamily::PilesAll
                } else {
                    DepartureRetainedFamily::Piles
                },
                foreign_occurrence,
                foreign_frame.clone(),
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("each chosen/unchosen reader retains the same process");
            runner
                .act(GameAction::ChoosePile { pile: PileSide::B })
                .unwrap();
            assert_eq!(
                runner.state().players[0].life,
                20 + 5 * i32::from(index + 1)
            );
            assert_eq!(
                runner.state().players[1].life,
                20 + 2 * i32::from(index + 1)
            );
        }
        departure_assert_program_complete(
            &runner,
            spell,
            5 * i32::from(subject_count),
            2 * i32::from(subject_count),
        );
    }
}

#[test]
fn departure_scoped_search_preparation_selection_and_custom_wire_keep_the_actual_owner() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ScopedLibrarySearchPhase, WaitingFor};
    let family = DepartureRetainedFamily::ScopedSearch;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_occurrence, foreign_frame) =
        foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 0, 0);
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_retained_family_runner(family, false, empty);
        let mut selected = Vec::new();
        for player in [PlayerId(0), PlayerId(1)] {
            let WaitingFor::SearchChoice {
                player: actual,
                cards,
                count,
                reveal,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("each real scoped player searches before delivery");
            };
            assert_eq!(*actual, player);
            assert_eq!(*count, 1);
            assert!(*reveal);
            assert_eq!(cards.len(), 8);
            let card = *cards.last().unwrap();
            let pending = runner
                .state()
                .pending_scoped_library_search
                .as_ref()
                .unwrap();
            assert!(matches!(
                pending.phase,
                ScopedLibrarySearchPhase::CollectSelections { .. }
            ));
            let occurrence = pending.return_result_occurrence.unwrap();
            assert_eq!(
                runner.state().return_result_frames[&occurrence].process,
                pending.ability.return_result_process
            );
            assert!(!runner.state().active_library_searches.is_empty());
            assert!(
                runner.state().players.iter().all(|p| p.hand.is_empty()),
                "announced choices do not deliver an earlier player's card"
            );
            if !empty {
                departure_assert_retained_hostile_reads(
                    &runner,
                    family,
                    foreign_occurrence,
                    foreign_frame.clone(),
                );
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect("custom scoped wire retains the actual bound filter, prepared choice and independent P");
            runner
                .act(GameAction::SelectCards { cards: vec![card] })
                .unwrap();
            selected.push((player, card));
        }
        for (player, card) in selected {
            assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
            assert_eq!(
                runner.state().players[player.0 as usize]
                    .hand
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![card]
            );
        }
        assert!(runner.state().pending_scoped_library_search.is_none());
        assert!(runner.state().active_library_searches.is_empty());
        assert!(runner.state().active_search_decision_controls.is_empty());
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
}

#[test]
fn departure_unless_discard_sacrifice_and_bounce_public_subpauses_retain_the_exact_process() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for family in [
        DepartureRetainedFamily::UnlessDiscard,
        DepartureRetainedFamily::UnlessSacrifice,
        DepartureRetainedFamily::UnlessBounce,
    ] {
        let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
        let (&foreign_occurrence, foreign_frame) =
            foreign.state().return_result_frames.iter().next().unwrap();
        let foreign_frame = foreign_frame.clone();
        departure_drain_retained_public_program(&mut foreign);
        departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
        for empty in [false, true] {
            let (mut runner, spell, subjects) =
                departure_retained_family_runner(family, false, empty);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPayment { .. }
            ));
            runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
            for remaining in [2, 1] {
                let (actual_remaining, candidates, body, occurrence) =
                    match &runner.state().waiting_for {
                        WaitingFor::WardDiscardChoice {
                            remaining,
                            cards,
                            pending_effect,
                            return_result_occurrence,
                            ..
                        } => (*remaining, cards, pending_effect, *return_result_occurrence),
                        WaitingFor::WardSacrificeChoice {
                            remaining,
                            permanents,
                            pending_effect,
                            return_result_occurrence,
                            ..
                        }
                        | WaitingFor::UnlessBounceChoice {
                            remaining,
                            permanents,
                            pending_effect,
                            return_result_occurrence,
                            ..
                        } => (
                            *remaining,
                            permanents,
                            pending_effect,
                            *return_result_occurrence,
                        ),
                        other => panic!(
                            "{family:?} really re-pauses for each selected cost member: {other:?}"
                        ),
                    };
                assert_eq!(actual_remaining, remaining);
                assert_eq!(candidates.len(), remaining as usize);
                assert!(candidates.iter().all(|card| subjects.contains(card)));
                assert_eq!(
                    runner.state().return_result_frames[&occurrence.unwrap()].process,
                    body.return_result_process
                );
                assert_eq!(runner.state().players[0].life, 20);
                assert_eq!(runner.state().players[1].life, 20);
                if !empty {
                    departure_assert_retained_hostile_reads(
                        &runner,
                        family,
                        foreign_occurrence,
                        foreign_frame.clone(),
                    );
                }
                let action = departure_retained_public_action(runner.state());
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved)
                    .expect("every actual payment member owner survives reload");
                runner.act(action).unwrap();
            }
            for card in subjects {
                assert_eq!(
                    runner.state().objects[&card].zone,
                    if matches!(family, DepartureRetainedFamily::UnlessBounce) {
                        Zone::Hand
                    } else {
                        Zone::Graveyard
                    }
                );
            }
            departure_assert_program_complete(&runner, spell, 0, 0);
        }
    }
}

#[test]
fn departure_record_match_ignores_arrival_and_reincarnated_object() {
    use engine::game::quantity::resolve_quantity_with_targets;
    use engine::types::ability::{
        AggregateFunction, ControllerRef, Effect, ObjectProperty, QuantityExpr, QuantityRef,
        ResolvedAbility, ReturnResultId, TargetFilter, TypedFilter, ZoneChangeRecordSource,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::InstructionResult;
    let (mut runner, spell, _) =
        departure_retained_family_runner(DepartureRetainedFamily::Branch, false, false);
    let (&occurrence, frame) = runner.state().return_result_frames.iter().next().unwrap();
    let mut view = ResolvedAbility::new(Effect::NoOp, vec![], spell, PlayerId(0));
    view.return_result_process = frame.process;
    let InstructionResult::ZoneChanges(original) = &frame.results[&ReturnResultId(7)] else {
        unreachable!()
    };
    let original = original.clone();
    assert_eq!(original.len(), 3);
    let mut state = runner.state().clone();
    state.active_return_result_occurrence = Some(occurrence);
    for record in &original {
        let object = state.objects.get_mut(&record.object_id).unwrap();
        object.incarnation += 1;
        object.controller = PlayerId(1 - record.controller.0);
        object.owner = PlayerId(1 - record.owner.0);
        object.power = Some(91);
        object.toughness = Some(93);
        object.card_types.core_types = vec![CoreType::Land];
        object.base_card_types = object.card_types.clone();
        let later = object.snapshot_public_characteristics();
        state.lki_cache.insert(record.object_id, later.clone());
        state
            .lki_by_incarnation
            .entry(record.object_id)
            .or_default()
            .insert(object.incarnation, later);
    }
    let InstructionResult::ZoneChanges(records) = state
        .return_result_frames
        .get_mut(&occurrence)
        .unwrap()
        .results
        .get_mut(&ReturnResultId(7))
        .unwrap()
    else {
        unreachable!()
    };
    for record in records.iter_mut() {
        record.arrival.controller = PlayerId(1 - record.controller.0);
        record.arrival.owner = PlayerId(1 - record.owner.0);
    }
    state.zone_changes_this_turn = records.iter().cloned().collect();
    let read = |source, player, function, property, to| QuantityExpr::Ref {
        qty: QuantityRef::ZoneChangeAggregate {
            source,
            from: Some(Zone::Battlefield),
            to: Some(to),
            filter: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::SpecificPlayer { id: player }),
            ),
            function,
            property,
        },
    };
    for property in [
        ObjectProperty::Power,
        ObjectProperty::Toughness,
        ObjectProperty::ManaValue,
    ] {
        for (player, sum, minimum, maximum) in [(PlayerId(0), 5, 2, 3), (PlayerId(1), 2, 2, 2)] {
            for (function, expected) in [
                (AggregateFunction::Sum, sum),
                (AggregateFunction::Min, minimum),
                (AggregateFunction::Max, maximum),
            ] {
                let expression = read(
                    ZoneChangeRecordSource::InstructionResult {
                        result: ReturnResultId(7),
                    },
                    player,
                    function,
                    property,
                    Zone::Exile,
                );
                assert_eq!(resolve_quantity_with_targets(&state, &expression, &view), expected, "departure type, controller, incarnation and property all use the same captured authority");
                let no_destination = read(
                    ZoneChangeRecordSource::InstructionResult {
                        result: ReturnResultId(7),
                    },
                    player,
                    function,
                    property,
                    Zone::Hand,
                );
                assert_eq!(
                    resolve_quantity_with_targets(&state, &no_destination, &view),
                    0
                );
            }
        }
    }
    for (player, expected) in [(PlayerId(0), 2), (PlayerId(1), 5)] {
        let journal = read(
            ZoneChangeRecordSource::ThisTurn,
            player,
            AggregateFunction::Sum,
            ObjectProperty::Power,
            Zone::Exile,
        );
        assert_eq!(
            resolve_quantity_with_targets(&state, &journal, &view),
            expected,
            "the established journal population deliberately keeps arrival-sensitive player axes"
        );
    }
    departure_drain_retained_public_program(&mut runner);
    departure_assert_program_complete(&runner, spell, 5, 2);
}

#[test]
fn owned_mass_result_survives_simultaneous_damage_replacement_remainder_and_restore() {
    use engine::game::scenario::{P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DamageModification, DamageTargetFilter,
        DamageTargetPlayerScope, Effect, PlayerFilter, QuantityExpr, ReplacementDefinition,
        ReturnResultId,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;
    // Supplemental admitted typed composition, not a claim about a printed card.
    // CR 608.2c + CR 120.4b: the complete exile result remains the same
    // instruction's authority while simultaneous damage resumes its raw tail.
    for restore in 0..3 {
        for empty in [false, true] {
            let (mut runner, spell, _) = departure_public_program_runner_with_setup(
                false,
                empty,
                2,
                |_| {
                    let mut damage = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::DamageEachPlayer {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player_filter: PlayerFilter::All,
                        },
                    );
                    damage.sub_ability = Some(Box::new(departure_gain_definition(7, P0)));
                    (damage, vec![])
                },
                |state, _| {
                    for (description, modification) in [
                        ("double current hit", DamageModification::Double),
                        (
                            "add one to current hit",
                            DamageModification::Plus {
                                value: QuantityExpr::Fixed { value: 1 },
                            },
                        ),
                    ] {
                        state.pending_damage_replacements.push(
                            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                                .damage_target_filter(DamageTargetFilter::Player {
                                    player: DamageTargetPlayerScope::Specific(P0),
                                })
                                .damage_modification(modification)
                                .damage_replacement_oneshot_shield()
                                .description(description.into()),
                        );
                    }
                },
            );
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!("the paid canonical cast must pause on the first player's two real shields");
            };
            assert_eq!(*player, P0);
            let index = candidates
                .iter()
                .position(|candidate| candidate.description == "double current hit")
                .expect("the actual doubling choice is offered");
            let continuation = runner.state().active_ability_continuation().unwrap();
            assert!(continuation.chain.context.damage_opportunity.is_some());
            let occurrence = continuation.return_result_occurrence.unwrap();
            let frame = &runner.state().return_result_frames[&occurrence];
            assert_eq!(frame.process, continuation.chain.return_result_process);
            let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
                unreachable!()
            };
            assert_eq!(records.len(), if empty { 0 } else { 3 });
            assert_eq!((runner.life(P0), runner.life(P1)), (20, 20));
            *runner.state_mut() = match restore {
                0 => runner.state().clone(),
                1 => serde_json::from_value(serde_json::to_value(runner.state()).unwrap())
                    .expect("the raw damage remainder retains its selected occurrence and process"),
                2 => serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(
                        runner.state().clone(),
                    ))
                    .unwrap(),
                )
                .expect("the wire damage remainder retains the same owned invocation")
                .into_game_state(),
                _ => unreachable!(),
            };
            runner.act(GameAction::ChooseReplacement { index }).unwrap();
            departure_assert_program_complete(&runner, spell, if empty { -3 } else { 2 }, -1);
            assert!(runner.state().pending_replacement.is_none());
            assert!(runner
                .state()
                .pending_damage_replacements
                .iter()
                .all(|shield| shield.is_consumed));
        }
    }
}

#[test]
fn oversimplify_result_publishes_once_after_complete_replacement_pauses_and_ignores_child_moves() {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ReplacementDefinition,
        ReplacementMode, ReturnResultId, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    use engine::types::replacements::ReplacementEvent;
    let (mut runner, spell, auxiliary) = departure_public_program_runner_with_setup(
        false,
        false,
        2,
        |scenario| {
            let auxiliary = scenario
                .add_creature(
                    PlayerId(0),
                    "Supplemental unrelated replacement child",
                    9,
                    9,
                )
                .id();
            (departure_branch_reader(), vec![auxiliary])
        },
        |state, victims| {
            let auxiliary = state
                .objects
                .values()
                .find(|object| object.name == "Supplemental unrelated replacement child")
                .unwrap()
                .id;
            let mut child: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZone", "origin":"Battlefield", "destination":"Exile"})).unwrap();
            let Effect::ChangeZone { target, .. } = &mut child else {
                unreachable!()
            };
            *target = TargetFilter::SpecificObject { id: auxiliary };
            for victim in victims {
                let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Exile)
                    .mode(ReplacementMode::MayCost {
                        cost: AbilityCost::PayLife {
                            amount: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        },
                        decline: Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            child.clone(),
                        ))),
                    });
                let object = state.objects.get_mut(victim).unwrap();
                object.replacement_definitions.push(replacement.clone());
                std::sync::Arc::make_mut(&mut object.base_replacement_definitions)
                    .push(replacement);
            }
        },
    );
    let auxiliary = auxiliary[0];
    for completed in 0..3 {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "every actual mass-move member must re-pause"
        );
        let pending = runner
            .state()
            .active_change_zone_frame()
            .unwrap()
            .pending
            .as_ref()
            .unwrap();
        let (occurrence, result) = pending.pending_return_result_producer.unwrap();
        assert_eq!(result, ReturnResultId(7));
        assert_eq!(
            runner.state().return_result_frames[&occurrence].process,
            pending.return_result_process
        );
        assert!(
            !runner.state().return_result_frames[&occurrence]
                .results
                .contains_key(&result),
            "a completed prefix is not the completed instruction"
        );
        assert_eq!(
            pending
                .logical_zone_change_group
                .all_origin_occurrences
                .len(),
            completed
        );
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(runner.state().players[1].life, 20);
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect("unpublished producer and exact logical delivery prefix survive every replacement re-pause");
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
    }
    assert_eq!(
        runner.state().objects[&auxiliary].zone,
        Zone::Exile,
        "the replacement child really moved its unrelated nine-power object"
    );
    assert!(runner.state().active_change_zone_frame().is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    assert_eq!(runner.state().return_result_frames.len(), 1);
    let frame = runner.state().return_result_frames.values().next().unwrap();
    assert_eq!(frame.results.len(), 1);
    let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
        unreachable!()
    };
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|record| record.object_id != auxiliary));
    assert_eq!(
        records
            .iter()
            .filter(|record| record.controller == PlayerId(0))
            .map(|record| record.power.unwrap())
            .sum::<i32>(),
        5
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.controller == PlayerId(1))
            .map(|record| record.power.unwrap())
            .sum::<i32>(),
        2
    );
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved)
        .expect("one final completed result publishes before its selected reader");
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    departure_assert_program_complete(&runner, spell, 5, 2);
}

#[test]
fn departure_mass_return_publishes_only_after_both_aura_host_pauses_and_remaining_members() {
    for restore in 0..3 {
        departure_mass_return_after_aura_pauses(restore);
    }
}

#[test]
fn departure_mass_selected_future_member_child_moves_do_not_rebind_primary() {
    for origin in [Zone::Battlefield, Zone::Graveyard] {
        for child_moves in 0..3 {
            for restore in 0..3 {
                departure_mass_child_member(origin, false, child_moves, false, restore, false);
            }
        }
    }
}

#[test]
fn departure_mass_current_primary_child_moves_do_not_rebind_primary() {
    for origin in [Zone::Battlefield, Zone::Graveyard] {
        for child_moves in 0..3 {
            for prevents in [false, true] {
                for restore in 0..3 {
                    departure_mass_child_member(
                        origin,
                        true,
                        child_moves,
                        prevents,
                        restore,
                        false,
                    );
                }
            }
        }
    }
}

#[test]
fn departure_mass_legacy_battlefield_publisher_recovers_first_pause_and_coherent_prefix() {
    for restore in 1..3 {
        departure_mass_child_member(Zone::Battlefield, false, 0, false, restore, true);
    }
}

#[test]
fn departure_mass_legacy_single_graveyard_request_recovers_its_exact_paused_identity() {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef,
        ReplacementDefinition, ReplacementMode, ReturnResultId, TargetFilter,
    };
    use engine::types::game_state::InstructionResult;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;
    for restore in 0..3 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let selected = scenario
            .add_creature_to_graveyard(P0, "Supplemental exact GY primary", 3, 3)
            .id();
        let mut root = departure_exile_definition(7);
        let Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            ..
        } = root.effect.as_mut()
        else {
            unreachable!()
        };
        *origin = Some(Zone::Graveyard);
        *destination = Zone::Battlefield;
        *target = TargetFilter::SpecificObject { id: selected };
        let mut reader = departure_gain_definition(7, P0);
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { from, to, .. },
                },
            ..
        } = reader.effect.as_mut()
        else {
            unreachable!()
        };
        *from = Some(Zone::Graveyard);
        *to = Some(Zone::Battlefield);
        let mut branch = departure_branch_reader();
        let Effect::ChooseOneOf { branches, .. } = branch.effect.as_mut() else {
            unreachable!()
        };
        branches[1] = reader;
        root.sub_ability = Some(Box::new(branch));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental exact GY legacy publisher", false)
            .with_ability_definition(root)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        for player in [P0, P1] {
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Library witness");
            }
        }
        let mut runner = scenario.build();
        let original = ObjectIncarnationRef::from_object(&runner.state().objects[&selected]);
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                decline: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::NoOp,
                ))),
            });
        let object = runner.state_mut().objects.get_mut(&selected).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
        runner.cast(spell).commit();
        for _ in 0..8 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("the real GY primary must pause");
        };
        assert_eq!(*player, P0);
        assert_eq!(candidates.len(), 2);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        let pending = runner
            .state()
            .active_change_zone_frame()
            .unwrap()
            .pending
            .as_ref()
            .unwrap();
        assert!(pending.remaining.is_empty());
        assert_eq!(pending.paused_current.as_ref().unwrap().member, original);
        if restore == 0 {
            runner
                .state_mut()
                .active_change_zone_frame_mut()
                .unwrap()
                .pending
                .as_mut()
                .unwrap()
                .logical_zone_change_group
                .instruction_deliveries = None;
            // Public actions do not perform save migration. A missing active
            // owner is refused before mutation, and restoring supplies it.
            let before = serde_json::to_value(runner.state()).unwrap();
            assert!(runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .is_err());
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
            *runner.state_mut() = serde_json::from_value(before)
                .expect("exact paused non-BF evidence normalizes on raw admission");
        } else {
            let mut saved = if restore == 1 {
                serde_json::to_value(runner.state()).unwrap()
            } else {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            };
            assert!(departure_remove_instruction_deliveries(&mut saved) > 0);
            *runner.state_mut() = if restore == 1 {
                serde_json::from_value(saved)
                    .expect("the paused member is an exact non-BF original witness")
            } else {
                serde_json::from_value::<ResolutionStateWire>(saved)
                    .expect("current wire admits exact non-BF paused evidence")
                    .into_game_state()
            };
        }
        let deliveries = runner
            .state()
            .active_change_zone_frame()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .logical_zone_change_group
            .instruction_deliveries
            .as_ref()
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].binding.reference, original);
        assert_eq!(deliveries[0].binding.expected_zone, Zone::Graveyard);
        assert_eq!(deliveries[0].completion, None);
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
        assert_eq!(runner.state().objects[&selected].zone, Zone::Battlefield);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
        let frame = runner.state().return_result_frames.values().next().unwrap();
        let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
            unreachable!()
        };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].object_id, selected);
        assert_eq!(records[0].from_zone, Some(Zone::Graveyard));
        assert_eq!(records[0].to_zone, Zone::Battlefield);
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        departure_assert_program_complete(&runner, spell, 3, 0);
    }
}

fn departure_mass_child_member(
    origin: Zone,
    current_primary: bool,
    child_moves: u8,
    prevents: bool,
    restore: u8,
    legacy: bool,
) {
    use engine::game::ability_utils::{build_target_slots, validate_targets_in_chain};
    use engine::game::targeting::resolved_targets;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, FilterProp, MultiTargetSpec,
        QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition, ReplacementMode,
        ReturnResultId, TargetChoiceTiming, TargetFilter, TargetRef, TriggerDefinition,
        TypedFilter,
    };
    use engine::types::game_state::{InstructionResult, ZoneMoveCompletion};
    use engine::types::identifiers::{ObjectIdentityBinding, ObjectIncarnationRef};
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;
    use engine::types::triggers::TriggerMode;

    // Supplemental typed instruction/replacement composition. No printed card
    // is being assigned these abilities. CR 400.7: even a return to the same
    // selected zone cannot turn a successor into the originally selected member.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selected = [
        scenario
            .add_creature(P0, "Supplemental primary A", 3, 3)
            .id(),
        scenario
            .add_creature(P0, "Supplemental future B", 2, 2)
            .id(),
        scenario
            .add_creature(P1, "Supplemental normal peer C", 2, 2)
            .id(),
    ];
    let invalidated = selected[usize::from(!current_primary)];
    let observer = scenario
        .add_enchantment_from_oracle(P0, "Supplemental departure observer", "")
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .destination(Zone::Hand)
                .valid_card(TargetFilter::SpecificObject { id: invalidated })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )),
        )
        .id();
    let mut root = departure_exile_definition(7);
    // CR 601.2c: announce A, B, C through the existing ordered target slots.
    // An unordered ChangeZoneAll scan could miss the child-before-primary seam.
    let mut effect: Effect = serde_json::from_value(serde_json::json!({
        "type": "ChangeZone", "origin": origin, "destination": "Exile"
    }))
    .unwrap();
    let Effect::ChangeZone { target, .. } = &mut effect else {
        unreachable!()
    };
    *target = TargetFilter::And {
        filters: vec![
            TargetFilter::Or {
                filters: selected
                    .iter()
                    .map(|id| TargetFilter::SpecificObject { id: *id })
                    .collect(),
            },
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::InZone { zone: origin }]),
            ),
        ],
    };
    root.effect = Box::new(effect);
    root.multi_target = Some(MultiTargetSpec::fixed(3, 3));
    let mut branch = departure_branch_reader();
    let Effect::ChooseOneOf { branches, .. } = branch.effect.as_mut() else {
        unreachable!()
    };
    {
        let gain = &mut branches[1];
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { from, .. },
                },
            ..
        } = gain.effect.as_mut()
        else {
            unreachable!()
        };
        *from = Some(origin);
        let second = gain.sub_ability.as_deref_mut().unwrap();
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { from, .. },
                },
            ..
        } = second.effect.as_mut()
        else {
            unreachable!()
        };
        *from = Some(origin);
    }
    root.sub_ability = Some(Box::new(branch));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental selected primary identity", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut runner = scenario.build();
    let mut setup = vec![];
    if origin == Zone::Graveyard {
        for id in selected {
            move_to_zone(runner.state_mut(), id, origin, &mut setup);
        }
    }
    let original: Vec<_> = selected
        .iter()
        .map(|id| {
            ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&runner.state().objects[id]),
                origin,
            )
        })
        .collect();
    // CR 616.1: the affected permanent's controller chooses; outside the
    // battlefield the card's owner chooses. Capture that authority before moves.
    let choosers: Vec<_> = selected
        .iter()
        .map(|id| {
            let object = &runner.state().objects[id];
            if origin == Zone::Battlefield {
                object.controller
            } else {
                object.owner
            }
        })
        .collect();
    let zone_move = |from: Zone, to: Zone| {
        let mut effect: Effect = serde_json::from_value(serde_json::json!({
            "type":"ChangeZone", "origin":from, "destination":to,
        }))
        .unwrap();
        let Effect::ChangeZone { target, .. } = &mut effect else {
            unreachable!()
        };
        *target = TargetFilter::SpecificObject { id: invalidated };
        AbilityDefinition::new(AbilityKind::Spell, effect)
    };
    let mut child = if child_moves == 0 {
        AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)
    } else {
        zone_move(origin, Zone::Hand)
    };
    if child_moves == 2 {
        child.sub_ability = Some(Box::new(zone_move(Zone::Hand, origin)));
    }
    let cost = AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value: 1 },
    };
    if current_primary {
        // Paying the outer replacement's life cost runs this existing life-loss
        // continuation before the parked zone proposal can physically deliver.
        // The two branches exercise a modified life event and a fully replaced
        // life event; both must preserve the same original zone identity.
        let replacement =
            ReplacementDefinition::new(ReplacementEvent::LoseLife).execute(child.clone());
        let replacement = if prevents {
            replacement
        } else {
            replacement.quantity_modification(QuantityModification::Plus { value: 0 })
        };
        let object = runner.state_mut().objects.get_mut(&observer).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
    }
    let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .active_zones(vec![origin])
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Exile)
        .mode(ReplacementMode::MayCost {
            cost: cost.clone(),
            decline: (!current_primary).then(|| Box::new(child.clone())),
        });
    for (id, replacement) in [
        (selected[0], replacement),
        (
            selected[2],
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .active_zones(vec![origin])
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Exile)
                .mode(ReplacementMode::MayCost {
                    cost,
                    decline: None,
                }),
        ),
    ] {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
    }
    {
        let committed = runner.cast(spell).target_objects(&selected).commit();
        let state = committed.state();
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.objects[&spell].zone, Zone::Stack);
        assert_eq!(state.players[0].mana_pool.total(), 0);
        assert_eq!(state.stack.len(), 1, "the paid spell actually committed");
        let entry = state.stack.back().unwrap();
        assert_eq!(entry.id, spell);
        assert_eq!(entry.source_id, spell);
        let engine::types::game_state::StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &entry.kind
        else {
            panic!("the actual cast must retain its selected-primary ability");
        };
        let Effect::ChangeZone {
            origin: announced_origin,
            destination: Zone::Exile,
            target,
            ..
        } = &ability.effect
        else {
            panic!("the committed spell must own the targeted zone instruction");
        };
        assert_eq!(*announced_origin, Some(origin));
        assert_eq!(ability.declares_return_result, Some(ReturnResultId(7)));
        assert_eq!(ability.target_choice_timing, TargetChoiceTiming::Stack);
        assert!(!ability.optional_targeting);
        assert_eq!(ability.multi_target, Some(MultiTargetSpec::fixed(3, 3)));
        // CR 601.2c: the real cast must announce all three original targets,
        // in order, with the engine's ordinary occurrence identity pins.
        let announced: Vec<_> = selected.iter().copied().map(TargetRef::Object).collect();
        let pins: Vec<_> = original
            .iter()
            .map(|binding| Some(binding.reference))
            .collect();
        assert_eq!(ability.targets, announced);
        assert_eq!(ability.chosen_target_slots.slots(), &[0, 1, 2]);
        assert_eq!(ability.target_pins, pins);
        let slots = build_target_slots(state, ability).expect("the cast owns three target slots");
        assert_eq!(slots.len(), 3);
        for slot in slots {
            assert!(!slot.optional, "all three announced targets are required");
            assert_eq!(slot.legal_targets.len(), selected.len());
            assert!(announced
                .iter()
                .all(|target| slot.legal_targets.contains(target)));
        }
        // CR 608.2b: these current originals must survive the same legality
        // check the stack uses before entering the actual zone resolver.
        let validated = validate_targets_in_chain(state, ability);
        assert_eq!(validated.targets, announced);
        assert_eq!(validated.target_pins, pins);
        assert_eq!(resolved_targets(&validated, target, state), announced);
        for binding in &original {
            let object = &state.objects[&binding.reference.object_id];
            assert_eq!(ObjectIncarnationRef::from_object(object), binding.reference);
            assert_eq!(object.zone, binding.expected_zone);
        }
    }
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the committed spell must reach A's replacement before another game decision: {:?}",
            runner.state().waiting_for
        );
        assert!(
            runner.state().stack.iter().any(|entry| entry.id == spell),
            "the announced three-target spell finished without reaching A's actual replacement"
        );
        runner.act(GameAction::PassPriority).unwrap();
    }
    for pause in 0..2 {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("the actual selected primary and the normal peer must each request a public replacement response");
        };
        let paused_index = if pause == 0 { 0 } else { 2 };
        assert_eq!(*player, choosers[paused_index]);
        assert_eq!(candidates.len(), 2);
        let pending = runner
            .state()
            .active_change_zone_frame()
            .unwrap()
            .pending
            .as_ref()
            .unwrap();
        let group = &pending.logical_zone_change_group;
        let paused = pending.paused_current.as_ref().unwrap();
        assert_eq!(paused.owner_group, Some(group.logical_group_id));
        assert_eq!(paused.member, original[paused_index].reference);
        assert!(matches!(
            paused.expected_event,
            engine::types::proposed_event::ProposedEvent::ZoneChange {
                object_id, from, to: Zone::Exile, ..
            } if object_id == selected[paused_index] && from == origin
        ));
        if pause == 0 {
            assert_eq!(
                pending
                    .remaining
                    .iter()
                    .map(|entry| entry.object_id)
                    .collect::<Vec<_>>(),
                selected[1..],
                "A's public replacement must precede B and C's primary requests"
            );
            for binding in &original {
                let object = &runner.state().objects[&binding.reference.object_id];
                assert_eq!(ObjectIncarnationRef::from_object(object), binding.reference);
                assert_eq!(object.zone, binding.expected_zone);
            }
            assert!(group
                .instruction_deliveries
                .as_ref()
                .unwrap()
                .iter()
                .all(|entry| entry.completion.is_none()));
        }
        assert_eq!(
            group
                .instruction_deliveries
                .as_ref()
                .unwrap()
                .iter()
                .map(|entry| entry.binding)
                .collect::<Vec<_>>(),
            original
        );
        let (occurrence, result) = pending.pending_return_result_producer.unwrap();
        assert!(!runner.state().return_result_frames[&occurrence]
            .results
            .contains_key(&result));
        if pause == 1 {
            assert!(
                pending.remaining.is_empty(),
                "C is the final selected primary"
            );
            assert_eq!(runner.state().objects[&selected[2]].zone, origin);
            let receipt = group
                .instruction_deliveries
                .as_ref()
                .unwrap()
                .iter()
                .find(|entry| entry.binding.reference.object_id == invalidated)
                .unwrap();
            assert_eq!(
                receipt.completion,
                Some(if child_moves == 0 {
                    ZoneMoveCompletion::Moved
                } else {
                    ZoneMoveCompletion::Remained
                })
            );
            if child_moves != 0 {
                assert_eq!(
                    runner.state().objects[&invalidated].zone,
                    if child_moves == 2 { origin } else { Zone::Hand }
                );
                assert_eq!(
                    runner.state().objects[&invalidated].incarnation,
                    original[usize::from(!current_primary)]
                        .reference
                        .incarnation
                        + u64::from(child_moves)
                );
                let child_records: Vec<_> = runner
                    .state()
                    .zone_changes_this_turn
                    .iter()
                    .filter(|record| {
                        record.object_id == invalidated && record.to_zone == Zone::Hand
                    })
                    .collect();
                assert_eq!(
                    child_records.len(),
                    1,
                    "the child really delivered the selected object away"
                );
                let primary_a = runner
                    .state()
                    .zone_changes_this_turn
                    .iter()
                    .position(|record| {
                        record.object_id == selected[0]
                            && record.from_zone == Some(origin)
                            && record.to_zone == Zone::Exile
                    });
                if current_primary {
                    assert!(
                        primary_a.is_none(),
                        "the child precedes A's physical primary delivery"
                    );
                } else {
                    let child_b = runner
                        .state()
                        .zone_changes_this_turn
                        .iter()
                        .position(|record| {
                            record.object_id == selected[1]
                                && record.from_zone == Some(origin)
                                && record.to_zone == Zone::Hand
                        })
                        .unwrap();
                    assert!(
                        primary_a.unwrap() < child_b,
                        "A delivers before its child invalidates future B"
                    );
                }
                if child_moves == 2 {
                    assert_eq!(
                        runner
                            .state()
                            .zone_changes_this_turn
                            .iter()
                            .filter(|record| record.object_id == invalidated
                                && record.from_zone == Some(Zone::Hand)
                                && record.to_zone == origin)
                            .count(),
                        1,
                        "the child really returned the same storage ID as a new incarnation"
                    );
                }
                if origin == Zone::Battlefield {
                    let slot = group
                        .prospective_battlefield_members
                        .iter()
                        .position(|member| {
                            member.identity == original[usize::from(!current_primary)].reference
                        })
                        .unwrap();
                    departure_assert_independent_retirement(
                        group,
                        original[usize::from(!current_primary)],
                        child_records[0],
                    );
                    assert!(group.post_event_member_contexts[slot].is_none());
                    departure_assert_child_observer_once(
                        runner.state(),
                        observer,
                        child_records[0],
                    );
                } else {
                    assert!(group.prospective_battlefield_members.is_empty());
                }
            }
        }
        let before_restore_group = group.clone();
        departure_assert_private_result_views(runner.state());
        if legacy {
            let mut saved = if restore == 1 {
                serde_json::to_value(runner.state()).unwrap()
            } else {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            };
            assert!(departure_remove_instruction_deliveries(&mut saved) > 0);
            *runner.state_mut() = if restore == 1 {
                serde_json::from_value(saved)
                    .expect("BF original membership recovers at both public pauses")
            } else {
                serde_json::from_value::<ResolutionStateWire>(saved)
                    .expect(
                        "wire normalizes actual retained BF frames before projection comparison",
                    )
                    .into_game_state()
            };
            assert_eq!(
                runner
                    .state()
                    .active_change_zone_frame()
                    .unwrap()
                    .pending
                    .as_ref()
                    .unwrap()
                    .logical_zone_change_group
                    .instruction_deliveries
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|entry| entry.binding)
                    .collect::<Vec<_>>(),
                original
            );
        } else {
            *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
        }
        assert_eq!(
            runner
                .state()
                .active_change_zone_frame()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .logical_zone_change_group,
            before_restore_group,
            "restore retains the original proof byte-for-byte"
        );
        runner
            .act(GameAction::ChooseReplacement {
                index: if pause == 0 && current_primary { 0 } else { 1 },
            })
            .unwrap();
    }
    assert_eq!(
        runner.state().objects[&selected[2]].zone,
        Zone::Exile,
        "the unaffected selected peer really delivers"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    let frame = runner.state().return_result_frames.values().next().unwrap();
    let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
        unreachable!()
    };
    let excluded = child_moves != 0;
    assert_eq!(records.len(), if excluded { 2 } else { 3 });
    assert_eq!(
        records.iter().any(|record| record.object_id == invalidated),
        !excluded
    );
    assert!(records.iter().any(|record| record.object_id == selected[2]
        && record.from_zone == Some(origin)
        && record.to_zone == Zone::Exile));
    assert!(
        records.iter().all(|record| record
            .trigger_source_context
            .as_ref()
            .is_some_and(|context| original.contains(&context.identity))),
        "only genuine primary records retain the original selected identities"
    );
    if excluded && origin == Zone::Battlefield {
        assert!(records
            .iter()
            .all(|record| !record.co_departed.contains(&invalidated)));
        let child = runner
            .state()
            .zone_changes_this_turn
            .iter()
            .find(|record| record.object_id == invalidated && record.to_zone == Zone::Hand)
            .unwrap();
        assert!(child.co_departed.iter().all(|id| !selected.contains(id)));
        departure_assert_child_observer_once(runner.state(), observer, child);
    }
    let completed_originals: Vec<_> = original
        .iter()
        .copied()
        .filter(|binding| !excluded || binding.reference.object_id != invalidated)
        .collect();
    departure_assert_original_bindings(
        records
            .iter()
            .map(|record| record.trigger_source_context.as_ref().unwrap().identity),
        &completed_originals,
    );
    if excluded {
        let object = &runner.state().objects[&invalidated];
        assert_eq!(
            object.zone,
            if child_moves == 2 || child_moves == 0 {
                origin
            } else {
                Zone::Hand
            }
        );
        assert_eq!(
            object.incarnation,
            original[usize::from(!current_primary)]
                .reference
                .incarnation
                + u64::from(child_moves),
            "finishing the genuine peer cannot move the same-ID successor"
        );
    }
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    // CR 603.10: the child's BF departure remains observable independently of
    // whether that object's primary instruction delivered an event.
    for _ in 0..12 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    let primary_gain = if excluded {
        if current_primary {
            2
        } else {
            3
        }
    } else {
        5
    };
    let observer_gain = i32::from(origin == Zone::Battlefield && child_moves != 0);
    assert_eq!(runner.state().objects[&observer].zone, Zone::Battlefield);
    departure_assert_program_complete(
        &runner,
        spell,
        primary_gain + observer_gain - i32::from(current_primary && !prevents),
        2,
    );
}

fn departure_assert_independent_retirement(
    group: &engine::types::game_state::LogicalZoneChangeGroup,
    original: engine::types::identifiers::ObjectIdentityBinding,
    actual: &engine::types::game_state::ZoneChangeRecord,
) {
    use engine::types::game_state::LogicalZoneChangeTerminalOutcome;
    let slot = group
        .prospective_battlefield_members
        .iter()
        .position(|member| member.identity == original.reference)
        .unwrap();
    let LogicalZoneChangeTerminalOutcome::RetiredByIndependentDeparture { departure } =
        &group.terminal_outcomes[slot]
    else {
        panic!("the original reservation must retain its independent physical departure");
    };
    // CR 400.7 + CR 603.10a: compare the actual original occurrence, never its
    // same-ID successor. Collector-derived stamps/concealment may change later.
    assert_eq!(
        departure.trigger_source_context.as_ref().unwrap().identity,
        original
    );
    let mut proof = departure.as_ref().clone();
    let mut finalized = actual.clone();
    proof.co_departed.clear();
    finalized.co_departed.clear();
    proof.trigger_source_context.as_mut().unwrap().face_down = false;
    finalized.trigger_source_context.as_mut().unwrap().face_down = false;
    assert_eq!(
        proof, finalized,
        "immutable occurrence key and complete stable core"
    );
    assert!(group.post_event_member_contexts[slot].is_none());
    assert!(
        group
            .all_origin_occurrences
            .iter()
            .all(|occurrence| !matches!(
                &occurrence.event, GameEvent::ZoneChanged { record, .. }
                if record.recorded_turn_number == actual.recorded_turn_number
                    && record.turn_zone_change_index == actual.turn_zone_change_index
            )),
        "a retirement witness is never a parent occurrence"
    );
    assert!(actual.co_departed.iter().all(|id| !group
        .prospective_battlefield_members
        .iter()
        .any(|member| member.identity.object_id == *id)));
    assert!(group
        .all_origin_occurrences
        .iter()
        .all(|occurrence| !matches!(
            &occurrence.event, GameEvent::ZoneChanged { record, .. }
            if record.co_departed.contains(&original.reference.object_id)
        )));
}

fn departure_assert_child_observer_once(
    state: &GameState,
    observer: ObjectId,
    child: &engine::types::game_state::ZoneChangeRecord,
) {
    let matches_event = |event: &GameEvent| {
        matches!(event,
        GameEvent::ZoneChanged { record, .. }
        if record.recorded_turn_number == child.recorded_turn_number
            && record.turn_zone_change_index == child.turn_zone_change_index
            && record.object_id == child.object_id)
    };
    let deferred = state
        .deferred_triggers
        .iter()
        .filter(|context| {
            context.pending.source_id == observer
                && context
                    .pending
                    .trigger_event
                    .as_ref()
                    .is_some_and(&matches_event)
        })
        .count();
    let pending = usize::from(state.pending_trigger.as_ref().is_some_and(|pending| {
        pending.source_id == observer && pending.trigger_event.as_ref().is_some_and(&matches_event)
    }));
    let placed = state
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                engine::types::game_state::StackEntryKind::TriggeredAbility {
                    source_id, trigger_event: Some(event), ..
                } if *source_id == observer && matches_event(event)
            )
        })
        .count();
    // CR 603.2c: this actual child occurrence has exactly one genuine observer.
    assert_eq!(deferred + pending + placed, 1);
}

fn departure_retained_primary(
    state: &GameState,
    original: engine::types::identifiers::ObjectIdentityBinding,
) -> (
    &engine::types::game_state::LogicalZoneChangeGroup,
    Option<&engine::types::game_state::PendingZoneChangeDelivery>,
) {
    use engine::types::resolution::ResolutionFrame;
    let mut owners = state.resolution_stack.iter().filter_map(|frame| {
        let (group, paused) = match frame {
            ResolutionFrame::ChangeZone(frame) => {
                let pending = frame.pending.as_ref()?;
                (
                    &pending.logical_zone_change_group,
                    pending.paused_current.as_ref(),
                )
            }
            ResolutionFrame::BatchDelivery(batch) => (
                &batch.logical_zone_change_group,
                batch.paused_current.as_ref(),
            ),
            _ => return None,
        };
        group
            .instruction_deliveries
            .as_ref()?
            .iter()
            .any(|delivery| delivery.binding == original)
            .then_some((group, paused))
    });
    let owner = owners
        .next()
        .expect("the original selected instruction remains resident");
    assert!(
        owners.next().is_none(),
        "the original has exactly one primary owner"
    );
    owner
}

fn departure_fixture_branch(
    selected: engine::types::ability::AbilityDefinition,
) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter,
    };
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![
                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                selected,
            ],
            kind: ChooseOneOfKind::Ordinary,
        },
    )
}

fn departure_assert_retirement_preflight(
    state: &GameState,
    original: engine::types::identifiers::ObjectIdentityBinding,
    proof: bool,
    delegation: bool,
) {
    use engine::types::game_state::LogicalZoneChangeTerminalOutcome;
    use engine::types::identifiers::LogicalZoneChangeGroupId;
    use engine::types::resolution::{ResolutionFrame, ResolutionStateWire};
    let group_id = departure_retained_primary(state, original)
        .0
        .logical_group_id;
    let action = match state.waiting_for {
        WaitingFor::ChooseOneOfBranch { .. } => GameAction::ChooseBranch { index: 1 },
        WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 1 },
        _ => panic!("hostile fixtures start from an actual public prompt"),
    };
    let faults: Vec<_> = (0..6)
        .filter(|fault| match fault {
            0 | 1 => !delegation,
            2 | 3 => proof,
            4 | 5 => delegation,
            _ => unreachable!(),
        })
        .collect();
    for fault in faults {
        let mut hostile = state.clone();
        let mut frames = vec![];
        while let Some(frame) = hostile.resolution_stack.last() {
            let kind = frame.kind();
            frames.push(hostile.resolution_stack.pop_expected(kind).unwrap());
        }
        let mut changed = false;
        for frame in &mut frames {
            if changed {
                break;
            }
            if fault <= 1 {
                if let ResolutionFrame::PostReplacement(drains) = frame {
                    let mut value = serde_json::to_value(&*drains).unwrap();
                    for drain in value["drains"].as_array_mut().unwrap() {
                        let Some(ids) = drain["independent_departure_ancestors"].as_array_mut()
                        else {
                            continue;
                        };
                        if !ids.contains(&serde_json::to_value(group_id).unwrap()) {
                            continue;
                        }
                        if fault == 0 {
                            ids.push(serde_json::to_value(group_id).unwrap());
                        } else {
                            *ids =
                                vec![serde_json::to_value(LogicalZoneChangeGroupId(u64::MAX - 1))
                                    .unwrap()];
                        }
                        changed = true;
                        break;
                    }
                    *drains = serde_json::from_value(value).unwrap();
                }
                continue;
            }
            if fault >= 4 {
                if let ResolutionFrame::BatchDelivery(batch) = frame {
                    if let Some(delegated) = &mut batch.delegated_primary {
                        if fault == 4 {
                            delegated.delivery.owner_group =
                                Some(LogicalZoneChangeGroupId(u64::MAX - 1));
                        } else {
                            delegated.components.push(delegated.components[0]);
                        }
                        changed = true;
                    }
                }
                continue;
            }
            let group = match frame {
                ResolutionFrame::ChangeZone(frame) => frame
                    .pending
                    .as_mut()
                    .map(|pending| &mut pending.logical_zone_change_group),
                ResolutionFrame::BatchDelivery(batch) => Some(&mut batch.logical_zone_change_group),
                _ => None,
            };
            let Some(group) = group else { continue };
            if group.logical_group_id != group_id {
                continue;
            }
            let slot = group
                .prospective_battlefield_members
                .iter()
                .position(|member| member.identity == original.reference)
                .unwrap();
            let LogicalZoneChangeTerminalOutcome::RetiredByIndependentDeparture { departure } =
                &mut group.terminal_outcomes[slot]
            else {
                panic!("the positive sibling reached an actual retirement proof");
            };
            if fault == 2 {
                departure.from_zone = Some(Zone::Graveyard);
            } else {
                group.post_event_member_contexts[slot] = departure.trigger_source_context.clone();
            }
            changed = true;
        }
        assert!(
            changed,
            "every corruption must reach its intended retained authority"
        );
        for frame in frames.into_iter().rev() {
            hostile.resolution_stack.push_inner(frame);
        }
        assert!(
            serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap()).is_err()
        );
        // The current writer validates before serializing. Start the hostile
        // read fixture from the valid envelope so both boundaries are tested.
        assert!(
            serde_json::to_value(ResolutionStateWire::from_game_state(hostile.clone())).is_err()
        );
        let mut wire = serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
            .expect("the intact paid prompt writes a valid current envelope");
        wire["resolution_frames"] = serde_json::to_value(&hostile.resolution_stack).unwrap();
        assert!(serde_json::from_value::<ResolutionStateWire>(wire).is_err());
        let mut runner = GameRunner::from_state(hostile);
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner.act(action.clone()).is_err());
        assert_eq!(
            serde_json::to_value(runner.state()).unwrap(),
            before,
            "malformed current authority refuses before public mutation"
        );
    }
}

fn departure_fixture_move(
    id: ObjectId,
    from: Zone,
    to: Zone,
) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
    let mut effect: Effect = serde_json::from_value(serde_json::json!({
        "type": "ChangeZone", "origin": from, "destination": to,
    }))
    .unwrap();
    let Effect::ChangeZone { target, .. } = &mut effect else {
        unreachable!()
    };
    *target = TargetFilter::SpecificObject { id };
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

fn departure_assert_bound_child_scope(
    state: &GameState,
    group_id: engine::types::identifiers::LogicalZoneChangeGroupId,
) {
    use engine::types::resolution::ResolutionFrame;
    let scopes: Vec<_> = state
        .resolution_stack
        .iter()
        .filter_map(|frame| {
            let ResolutionFrame::PostReplacement(_) = frame else {
                return None;
            };
            Some(serde_json::to_value(frame).unwrap())
        })
        .collect();
    assert!(
        scopes.iter().any(
            |frame| frame["data"]["drains"]
                .as_array()
                .unwrap()
                .iter()
                .any(|drain| drain["independent_departure_ancestors"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&serde_json::to_value(group_id).unwrap())))
        ),
        "the actual retained child root is bound to this exact parent group"
    );
}

#[test]
fn departure_mass_later_branch_answer_retires_original_before_second_pause() {
    for restore in 0..3 {
        for movement in 0..3 {
            for deep in [false, true] {
                departure_later_branch_retirement(false, movement, deep, restore);
            }
        }
    }
}

#[test]
fn departure_batch_later_branch_answer_retires_original_before_second_pause() {
    for restore in 0..3 {
        for movement in 0..3 {
            for deep in [false, true] {
                departure_later_branch_retirement(true, movement, deep, restore);
            }
        }
    }
}

fn departure_later_branch_retirement(batch: bool, movement: u8, deep: bool, restore: u8) {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, MultiTargetSpec, QuantityExpr,
        ReplacementDefinition, ReplacementMode, ReturnResultId, TargetFilter, TriggerDefinition,
    };
    use engine::types::game_state::{
        InstructionResult, LogicalZoneChangeTerminalOutcome, ZoneMoveCompletion,
    };
    use engine::types::identifiers::{ObjectIdentityBinding, ObjectIncarnationRef};
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    use engine::types::triggers::TriggerMode;
    // CR 608.2c + CR 400.7: this synthetic instruction owns its announced
    // originals; independently chosen child instructions cannot rebind them.
    let destination = if batch { Zone::Hand } else { Zone::Exile };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selected = [
        scenario.add_creature(P0, "Later A", 3, 3).id(),
        scenario.add_creature(P0, "Later B", 2, 2).id(),
        scenario.add_creature(P1, "Later C", 2, 2).id(),
    ];
    let unselected = scenario
        .add_creature(P0, "Unselected later creature", 9, 9)
        .id();
    let observer = scenario
        .add_enchantment_from_oracle(P0, "Later child observer", "")
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .valid_card(TargetFilter::SpecificObject { id: selected[1] })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )),
        )
        .id();
    let mut root = departure_exile_definition(7);
    let filter = TargetFilter::Or {
        filters: selected
            .iter()
            .map(|id| TargetFilter::SpecificObject { id: *id })
            .collect(),
    };
    if batch {
        root.effect = Box::new(Effect::BounceAll {
            target: filter,
            destination: Some(destination),
            count: None,
        });
    } else {
        let mut effect: Effect = serde_json::from_value(serde_json::json!({
            "type": "ChangeZone", "origin": "Battlefield", "destination": destination,
        }))
        .unwrap();
        let Effect::ChangeZone { target, .. } = &mut effect else {
            unreachable!()
        };
        // CR 601.2c: announced targets use an enumerable creature population;
        // SpecificObject is a resolution-time binding, not a target selector.
        *target = TargetFilter::Typed(engine::types::ability::TypedFilter::creature());
        root.effect = Box::new(effect);
        root.multi_target = Some(MultiTargetSpec::fixed(3, 3));
    }
    let mut reader = departure_dual_reader();
    let mut current = Some(&mut reader);
    while let Some(definition) = current {
        if let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: engine::types::ability::QuantityRef::ZoneChangeAggregate { to, .. },
                },
            ..
        } = definition.effect.as_mut()
        {
            *to = Some(destination);
        }
        current = definition.sub_ability.as_deref_mut();
    }
    root.sub_ability = Some(Box::new(departure_fixture_branch(reader)));
    let spell = scenario
        .add_spell_to_hand(P0, "Later branch selected instruction", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Later library");
        }
    }
    let mut runner = scenario.build();
    let originals: Vec<_> = selected
        .iter()
        .map(|id| {
            ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&runner.state().objects[id]),
                Zone::Battlefield,
            )
        })
        .collect();
    let mut moving = if movement == 0 {
        AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)
    } else {
        departure_fixture_move(selected[1], Zone::Battlefield, Zone::Graveyard)
    };
    let second = if movement == 0 {
        departure_fixture_branch(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
    } else {
        moving.declares_return_result = Some(ReturnResultId(91));
        let mut child_reader = departure_gain_definition(91, P0);
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: engine::types::ability::QuantityRef::ZoneChangeAggregate { to, .. },
                },
            ..
        } = child_reader.effect.as_mut()
        else {
            unreachable!()
        };
        *to = Some(Zone::Graveyard);
        departure_fixture_branch(child_reader)
    };
    if movement == 2 {
        let mut back = departure_fixture_move(selected[1], Zone::Graveyard, Zone::Battlefield);
        back.sub_ability = Some(Box::new(second));
        moving.sub_ability = Some(Box::new(back));
    } else {
        moving.sub_ability = Some(Box::new(second));
    }
    // The deep sibling enters a second actual replacement dispatch, then resumes
    // an ordinary written continuation after that nested child finishes.
    let child = if deep {
        let replacement = ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(engine::types::ability::QuantityModification::Plus { value: 0 })
            .execute(departure_fixture_branch(moving));
        let object = runner.state_mut().objects.get_mut(&observer).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
        // CR 119.3: the nested modified loss pays exactly one real life.
        let mut loss = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::SpecificPlayer { id: P0 }),
            },
        );
        loss.sub_ability = Some(Box::new(departure_fixture_branch(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::NoOp,
        ))));
        departure_fixture_branch(loss)
    } else {
        departure_fixture_branch(moving)
    };
    for (id, decline) in [(selected[0], Some(Box::new(child))), (selected[2], None)] {
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .active_zones(vec![Zone::Battlefield])
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(destination)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                decline,
            });
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
    }
    if batch {
        runner.cast(spell).commit();
    } else {
        runner.cast(spell).target_objects(&selected).commit();
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!("the paid instruction reaches its first A replacement");
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates.len(), 2);
    assert!(
        matches!(runner.state().pending_replacement.as_ref().unwrap().proposed,
        engine::types::proposed_event::ProposedEvent::ZoneChange { object_id, .. } if object_id == selected[0])
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    let group_id = departure_retained_primary(runner.state(), originals[1])
        .0
        .logical_group_id;
    let parent_occurrence = runner
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| {
            use engine::types::resolution::ResolutionFrame;
            match frame {
            ResolutionFrame::ChangeZone(frame) => {
                let pending = frame.pending.as_ref()?;
                (pending.logical_zone_change_group.logical_group_id == group_id)
                    .then_some(pending.pending_return_result_producer?.0)
            }
            ResolutionFrame::BatchDelivery(pending)
                if pending.logical_zone_change_group.logical_group_id == group_id => {
                match pending.completion.as_ref()? {
                    engine::types::game_state::BatchCompletion::RecordInstructionZoneResult {
                        occurrence_id, ..
                    } => Some(*occurrence_id),
                    _ => None,
                }
            }
            _ => None,
        }
        })
        .expect("the exact retained parent owns the unpublished result address");
    let parent_process = runner.state().return_result_frames[&parent_occurrence].process;
    assert!(parent_process.is_some());
    for _ in 0..=usize::from(deep) {
        let WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } = &runner.state().waiting_for
        else {
            panic!("a real child branch must pause before any B departure");
        };
        assert_eq!(*player, P0);
        assert_eq!(branches.len(), 2);
        let (group, _) = departure_retained_primary(runner.state(), originals[1]);
        assert_eq!(group.logical_group_id, group_id);
        let slot = group
            .prospective_battlefield_members
            .iter()
            .position(|m| m.identity == originals[1].reference)
            .unwrap();
        assert_eq!(
            group.terminal_outcomes[slot],
            LogicalZoneChangeTerminalOutcome::Pending
        );
        assert!(group
            .instruction_deliveries
            .as_ref()
            .unwrap()
            .iter()
            .find(|d| d.binding == originals[1])
            .unwrap()
            .completion
            .is_none());
        assert_eq!(
            runner.state().objects[&selected[1]].incarnation,
            originals[1].reference.incarnation
        );
        assert!(runner
            .state()
            .return_result_frames
            .values()
            .all(|frame| !frame.results.contains_key(&ReturnResultId(7))));
        departure_assert_bound_child_scope(runner.state(), group_id);
        departure_assert_retirement_preflight(runner.state(), originals[1], false, false);
        *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { player: P0, .. }
    ));
    let (group, _) = departure_retained_primary(runner.state(), originals[1]);
    assert_eq!(group.logical_group_id, group_id);
    assert_eq!(
        runner.state().return_result_frames[&parent_occurrence].process,
        parent_process
    );
    assert!(runner
        .state()
        .return_result_frames
        .values()
        .all(|frame| !frame.results.contains_key(&ReturnResultId(7))));
    let saved_group = group.clone();
    let foreign_process = if movement != 0 {
        let (id, frame) = runner
            .state()
            .return_result_frames
            .iter()
            .find(|(_, frame)| frame.results.contains_key(&ReturnResultId(91)))
            .unwrap();
        assert_ne!(
            frame.process, parent_process,
            "the actual completed child instruction has an independent process"
        );
        Some((*id, frame.clone()))
    } else {
        None
    };
    if movement != 0 {
        let actual = runner
            .state()
            .zone_changes_this_turn
            .iter()
            .find(|record| record.object_id == selected[1] && record.to_zone == Zone::Graveyard)
            .unwrap();
        departure_assert_independent_retirement(group, originals[1], actual);
        departure_assert_child_observer_once(runner.state(), observer, actual);
        assert!(group
            .instruction_deliveries
            .as_ref()
            .unwrap()
            .iter()
            .find(|d| d.binding == originals[1])
            .unwrap()
            .completion
            .is_none());
        assert_eq!(
            runner.state().objects[&selected[1]].incarnation,
            originals[1].reference.incarnation + u64::from(movement)
        );
        departure_assert_retirement_preflight(runner.state(), originals[1], true, false);
    }
    *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
    assert_eq!(
        departure_retained_primary(runner.state(), originals[1]).0,
        &saved_group,
        "restore preserves the complete immutable proof before parent drain"
    );
    if let Some((id, frame)) = &foreign_process {
        assert_eq!(
            &runner.state().return_result_frames[id],
            frame,
            "retirement delivery and restore preserve the independent child's result authority"
        );
    }
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    if deep {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseOneOfBranch { player: P0, .. }
            ),
            "the ordinary continuation after the nested dispatch must also re-pause"
        );
        assert_eq!(
            departure_retained_primary(runner.state(), originals[1])
                .0
                .logical_group_id,
            group_id
        );
        if movement != 0 {
            let actual = runner
                .state()
                .zone_changes_this_turn
                .iter()
                .find(|record| record.object_id == selected[1] && record.to_zone == Zone::Graveyard)
                .unwrap();
            departure_assert_independent_retirement(
                departure_retained_primary(runner.state(), originals[1]).0,
                originals[1],
                actual,
            );
        }
        *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    }
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!("the normal selected C must still reach its real replacement");
    };
    assert_eq!(*player, P1);
    assert_eq!(candidates.len(), 2);
    let (group, paused) = departure_retained_primary(runner.state(), originals[1]);
    assert_eq!(paused.unwrap().member, originals[2].reference);
    assert_eq!(
        group
            .instruction_deliveries
            .as_ref()
            .unwrap()
            .iter()
            .find(|d| d.binding == originals[1])
            .unwrap()
            .completion,
        Some(if movement == 0 {
            ZoneMoveCompletion::Moved
        } else {
            ZoneMoveCompletion::Remained
        })
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { player: P0, .. }
    ));
    let frame = runner
        .state()
        .return_result_frames
        .values()
        .find(|frame| frame.results.contains_key(&ReturnResultId(7)))
        .unwrap();
    let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
        unreachable!()
    };
    departure_assert_original_bindings(
        records
            .iter()
            .map(|r| r.trigger_source_context.as_ref().unwrap().identity),
        &originals
            .iter()
            .copied()
            .filter(|binding| movement == 0 || binding.reference.object_id != selected[1])
            .collect::<Vec<_>>(),
    );
    assert!(records.iter().all(|record| record.to_zone == destination));
    if movement != 0 {
        assert!(records
            .iter()
            .all(|record| !record.co_departed.contains(&selected[1])));
    }
    assert_eq!(runner.state().objects[&selected[2]].zone, destination);
    assert_eq!(runner.state().objects[&unselected].zone, Zone::Battlefield);
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .all(|record| record.object_id != unselected));
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    departure_assert_program_complete(
        &runner,
        spell,
        if movement == 0 { 5 } else { 6 } - i32::from(deep),
        2,
    );
}

#[test]
fn departure_merged_primary_survives_component_choices_and_restore() {
    for restore in 0..3 {
        departure_merged_primary(false, 0, restore);
    }
}

#[test]
fn departure_merged_primary_excludes_independent_child_invalidation() {
    for restore in 0..3 {
        for movement in 0..3 {
            departure_merged_primary(true, movement, restore);
        }
    }
}

fn departure_merged_primary(independent: bool, movement: u8, restore: u8) {
    use engine::game::merge::MergeSide;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, MultiTargetSpec, QuantityExpr,
        ReplacementDefinition, ReplacementMode, ReturnResultId, TargetFilter, TriggerDefinition,
    };
    use engine::types::game_state::{
        InstructionResult, LogicalZoneChangeTerminalOutcome, ZoneMoveCompletion,
    };
    use engine::types::identifiers::{ObjectIdentityBinding, ObjectIncarnationRef};
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionFrame;
    use engine::types::triggers::TriggerMode;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let survivor = scenario
        .add_creature(P0, "Merged selected survivor", 3, 3)
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "Earlier physical component", 4, 4)
        .with_keyword(engine::types::keywords::Keyword::Mutate(ManaCost::zero()))
        .with_mana_cost(ManaCost::zero())
        .id();
    let last = scenario
        .add_creature_to_hand(P0, "Later commander component", 5, 5)
        .with_keyword(engine::types::keywords::Keyword::Mutate(ManaCost::zero()))
        .with_mana_cost(ManaCost::zero())
        .id();
    let peer = scenario
        .add_creature(P1, "Ordinary selected peer", 2, 2)
        .id();
    let unselected = scenario
        .add_creature(P0, "Unselected merged peer", 9, 9)
        .id();
    let observer = scenario
        .add_enchantment_from_oracle(P0, "Merged stable observer", "")
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .valid_card(TargetFilter::SpecificObject { id: survivor })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )),
        )
        .id();
    let mut root = departure_fixture_move(survivor, Zone::Battlefield, Zone::Hand);
    let Effect::ChangeZone { target, .. } = root.effect.as_mut() else {
        unreachable!()
    };
    // CR 601.2c: choose exactly the survivor and peer from legal creatures.
    *target = TargetFilter::Typed(engine::types::ability::TypedFilter::creature());
    root.multi_target = Some(MultiTargetSpec::fixed(2, 2));
    root.declares_return_result = Some(ReturnResultId(7));
    // Read only the ordinary peer's known two power. The expected two-member
    // result below derives from CR 730.3, without a merged-power premise.
    let mut reader = departure_gain_definition(7, P0);
    let Effect::GainLife {
        amount:
            QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::ZoneChangeAggregate { to, filter, .. },
            },
        ..
    } = reader.effect.as_mut()
    else {
        unreachable!()
    };
    *to = Some(Zone::Hand);
    *filter = TargetFilter::SpecificObject { id: peer };
    root.sub_ability = Some(Box::new(departure_fixture_branch(reader)));
    let spell = scenario
        .add_spell_to_hand(P0, "Merged selected pair", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Merged library");
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    runner
        .state_mut()
        .objects
        .get_mut(&first)
        .unwrap()
        .is_commander = !independent;
    runner
        .state_mut()
        .objects
        .get_mut(&last)
        .unwrap()
        .is_commander = true;
    // CR 702.140a + CR 702.140c + CR 730.2b: each resolving mutating
    // creature has left the stack before the public side choice absorbs it.
    // The component ordering then brackets the survivor's own departure.
    for (component, side) in [(first, MergeSide::Top), (last, MergeSide::Bottom)] {
        runner
            .cast(component)
            .casting_variant(engine::types::game_state::CastingVariant::Mutate)
            .target_objects(&[survivor])
            .commit();
        assert_eq!(runner.state().objects[&component].zone, Zone::Stack);
        runner.advance_until_stack_empty();
        let WaitingFor::MutateMergeChoice {
            player,
            merging_id,
            target_id,
        } = runner.state().waiting_for
        else {
            panic!("the real mutating spell must reach its public merge choice");
        };
        assert_eq!(player, P0);
        assert_eq!(merging_id, component);
        assert_eq!(target_id, survivor);
        assert!(runner.state().stack.is_empty());
        runner
            .act(GameAction::ChooseMutateMergeSide { side })
            .unwrap();
        assert!(!runner.state().players[0].hand.contains(&component));
        assert!(!runner.state().battlefield.contains(&component));
        assert_eq!(runner.state().objects[&component].zone, Zone::Battlefield);
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
    assert_eq!(
        runner.state().objects[&survivor].merged_components,
        vec![first, survivor, last]
    );
    assert!(!runner.state().battlefield.contains(&first));
    assert!(!runner.state().battlefield.contains(&last));
    let originals: Vec<_> = [survivor, peer]
        .iter()
        .map(|id| {
            ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&runner.state().objects[id]),
                Zone::Battlefield,
            )
        })
        .collect();
    if independent {
        let mut child = if movement == 0 {
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)
        } else {
            departure_fixture_move(survivor, Zone::Battlefield, Zone::Exile)
        };
        let pause =
            departure_fixture_branch(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
        if movement == 2 {
            let mut back = departure_fixture_move(survivor, Zone::Exile, Zone::Battlefield);
            back.sub_ability = Some(Box::new(pause));
            child.sub_ability = Some(Box::new(back));
        } else {
            child.sub_ability = Some(Box::new(pause));
        }
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .active_zones(vec![Zone::Battlefield])
            .valid_card(TargetFilter::SpecificObject { id: first })
            .destination_zone(Zone::Hand)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                decline: Some(Box::new(child)),
            });
        let object = runner.state_mut().objects.get_mut(&observer).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
    }
    runner
        .cast(spell)
        .target_objects(&[survivor, peer])
        .commit();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!("delegation must reach the first component's genuine public choice");
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates.len(), 2);
    assert!(
        matches!(runner.state().pending_replacement.as_ref().unwrap().proposed,
        engine::types::proposed_event::ProposedEvent::ZoneChange { object_id, to: Zone::Hand, .. } if object_id == first)
    );
    if !independent {
        assert_eq!(candidates[0].description, "Move to command zone");
        assert_eq!(candidates[1].description, "Put into hand");
    }
    let (group, paused) = departure_retained_primary(runner.state(), originals[0]);
    let outer_id = group.logical_group_id;
    assert_eq!(group.instruction_deliveries.as_ref().unwrap().len(), 2);
    let initial = paused.unwrap();
    assert_eq!(initial.owner_group, Some(outer_id));
    assert_eq!(initial.member, originals[0].reference);
    assert!(initial.delivery_events.is_empty());
    assert!(initial.terminal_completion.is_none());
    assert!(
        runner.state().objects[&survivor]
            .merged_components
            .is_empty(),
        "delegation has already split the real wrapper"
    );
    let component_batch = runner
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            ResolutionFrame::BatchDelivery(batch) if batch.delegated_primary.is_some() => {
                Some(batch)
            }
            _ => None,
        })
        .expect("the exact retained component batch owns the delegated boundary");
    assert!(component_batch
        .logical_zone_change_group
        .prospective_battlefield_members
        .is_empty());
    departure_assert_retirement_preflight(runner.state(), originals[0], false, true);
    assert!(component_batch
        .logical_zone_change_group
        .instruction_deliveries
        .as_ref()
        .unwrap()
        .is_empty());
    *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    if independent {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { player: P0, .. }
        ));
        let (group, paused) = departure_retained_primary(runner.state(), originals[0]);
        assert_eq!(group.logical_group_id, outer_id);
        assert!(paused.unwrap().delivery_events.is_empty());
        if movement != 0 {
            let actual = runner
                .state()
                .zone_changes_this_turn
                .iter()
                .find(|record| record.object_id == survivor && record.to_zone == Zone::Exile)
                .unwrap();
            departure_assert_independent_retirement(group, originals[0], actual);
            departure_assert_child_observer_once(runner.state(), observer, actual);
        }
        let before = group.clone();
        *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
        assert_eq!(
            departure_retained_primary(runner.state(), originals[0]).0,
            &before
        );
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    }
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!("the later commander must pause after the survivor's delegated request");
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].description, "Move to command zone");
    assert_eq!(candidates[1].description, "Put into hand");
    assert!(
        matches!(runner.state().pending_replacement.as_ref().unwrap().proposed,
        engine::types::proposed_event::ProposedEvent::ZoneChange { object_id, .. } if object_id == last)
    );
    let (group, paused) = departure_retained_primary(runner.state(), originals[0]);
    assert_eq!(group.logical_group_id, outer_id);
    let receipt = paused.unwrap();
    assert_eq!(receipt.owner_group, Some(outer_id));
    let excluded = independent && movement != 0;
    assert_eq!(
        receipt.terminal_completion,
        Some(if excluded {
            ZoneMoveCompletion::Remained
        } else {
            ZoneMoveCompletion::Moved
        })
    );
    assert_eq!(receipt.delivery_events.len(), usize::from(!excluded));
    if !excluded {
        assert!(
            matches!(&receipt.delivery_events[0], GameEvent::ZoneChanged { record, .. }
            if record.object_id == survivor && record.from_zone == Some(Zone::Battlefield) && record.to_zone == Zone::Hand)
        );
        assert!(!group.terminal_outcomes.iter().any(|outcome| matches!(
            outcome,
            LogicalZoneChangeTerminalOutcome::RetiredByIndependentDeparture { .. }
        )));
    }
    assert!(runner
        .state()
        .return_result_frames
        .values()
        .all(|frame| !frame.results.contains_key(&ReturnResultId(7))));
    let saved_receipt = receipt.clone();
    departure_assert_retirement_preflight(runner.state(), originals[0], excluded, true);
    *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
    assert_eq!(
        departure_retained_primary(runner.state(), originals[0])
            .1
            .unwrap(),
        &saved_receipt
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&last].zone, Zone::Command);
    for (id, destination) in [(first, Zone::Hand), (last, Zone::Command)] {
        let components: Vec<_> = runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|record| record.object_id == id && record.to_zone == destination)
            .collect();
        assert_eq!(components.len(), 1);
        assert_eq!(
            components[0].from_zone, None,
            "absorbed cards are real auxiliary deliveries"
        );
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { player: P0, .. }
    ));
    let frame = runner
        .state()
        .return_result_frames
        .values()
        .find(|frame| frame.results.contains_key(&ReturnResultId(7)))
        .unwrap();
    let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
        unreachable!()
    };
    // CR 730.3: the typed pair selects one merged permanent and one peer.
    departure_assert_original_bindings(
        records
            .iter()
            .map(|record| record.trigger_source_context.as_ref().unwrap().identity),
        &originals
            .iter()
            .copied()
            .filter(|binding| !excluded || binding.reference.object_id != survivor)
            .collect::<Vec<_>>(),
    );
    assert!(records
        .iter()
        .all(|record| record.from_zone == Some(Zone::Battlefield) && record.to_zone == Zone::Hand));
    assert!(records
        .iter()
        .all(|record| record.object_id != first && record.object_id != last));
    assert_eq!(runner.state().objects[&peer].zone, Zone::Hand);
    if excluded {
        assert_eq!(
            runner.state().objects[&survivor].zone,
            if movement == 2 {
                Zone::Battlefield
            } else {
                Zone::Exile
            }
        );
        assert_eq!(
            runner.state().objects[&survivor].incarnation,
            originals[0].reference.incarnation + u64::from(movement)
        );
        assert!(records
            .iter()
            .all(|record| !record.co_departed.contains(&survivor)));
    } else {
        assert_eq!(runner.state().objects[&survivor].zone, Zone::Hand);
        assert_eq!(
            runner
                .state()
                .zone_changes_this_turn
                .iter()
                .filter(|record| record.object_id == survivor
                    && record.from_zone == Some(Zone::Battlefield)
                    && record.to_zone == Zone::Hand)
                .count(),
            1
        );
    }
    let actual = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == survivor && record.from_zone == Some(Zone::Battlefield))
        .unwrap();
    departure_assert_child_observer_once(runner.state(), observer, actual);
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().objects[&unselected].zone, Zone::Battlefield);
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .all(|record| record.object_id != unselected));
    departure_assert_program_complete(&runner, spell, 3, 0);
}

fn departure_restore_mass_owner(state: &GameState, restore: u8) -> GameState {
    use engine::types::resolution::ResolutionStateWire;
    match restore {
        0 => state.clone(),
        1 => serde_json::from_value(serde_json::to_value(state).unwrap())
            .expect("raw state retains the exact original instruction population"),
        2 => serde_json::from_value::<ResolutionStateWire>(
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap(),
        )
        .expect("current wire retains the exact original instruction population")
        .into_game_state(),
        _ => unreachable!(),
    }
}

fn departure_remove_instruction_deliveries(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let removed = usize::from(fields.remove("instruction_deliveries").is_some());
            removed
                + fields
                    .values_mut()
                    .map(departure_remove_instruction_deliveries)
                    .sum::<usize>()
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(departure_remove_instruction_deliveries)
            .sum(),
        _ => 0,
    }
}

fn departure_assert_original_bindings(
    actual: impl Iterator<Item = engine::types::identifiers::ObjectIdentityBinding>,
    expected: &[engine::types::identifiers::ObjectIdentityBinding],
) {
    use std::collections::HashSet;
    // CR 400.7: compare the full original incarnation and zone, with no ordering claim.
    let actual: Vec<_> = actual.collect();
    assert_eq!(actual.len(), expected.len());
    let unique: HashSet<_> = actual.iter().copied().collect();
    assert_eq!(unique.len(), actual.len(), "original bindings are unique");
    assert_eq!(unique, expected.iter().copied().collect::<HashSet<_>>());
}

fn departure_mass_return_after_aura_pauses(restore: u8) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        Effect, QuantityExpr, QuantityRef, ReturnResultId, TargetFilter, TargetRef, TypedFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    let (mut foreign, foreign_spell, _) =
        departure_retained_family_runner(DepartureRetainedFamily::Branch, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let mut auras = vec![];
    for name in [
        "Supplemental canonical returned Aura A",
        "Supplemental canonical returned Aura B",
    ] {
        auras.push(
            scenario
                .add_enchantment_from_oracle(P0, name, "")
                .with_subtypes(vec!["Aura"])
                .with_keyword(Keyword::Enchant(TargetFilter::Typed(
                    TypedFilter::creature(),
                )))
                .id(),
        );
    }
    let hosts = [
        scenario
            .add_creature(P0, "Supplemental first host", 11, 11)
            .id(),
        scenario
            .add_creature(P0, "Supplemental second host", 13, 13)
            .id(),
    ];
    let returned = [
        scenario
            .add_creature_to_graveyard(P0, "Centaur Courser", 3, 3)
            .id(),
        scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .id(),
        scenario
            .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
            .id(),
    ];
    let all: Vec<_> = auras.iter().chain(returned.iter()).copied().collect();
    let mut root = departure_exile_definition(7);
    let Effect::ChangeZoneAll {
        origin,
        destination,
        target,
        ..
    } = root.effect.as_mut()
    else {
        unreachable!()
    };
    *origin = Some(Zone::Graveyard);
    *destination = Zone::Battlefield;
    *target = TargetFilter::Or {
        filters: all
            .iter()
            .map(|id| TargetFilter::SpecificObject { id: *id })
            .collect(),
    };
    let mut first = departure_gain_definition(7, P0);
    let mut second = departure_gain_definition(7, P1);
    for gain in [&mut first, &mut second] {
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { from, to, .. },
                },
            ..
        } = gain.effect.as_mut()
        else {
            unreachable!()
        };
        *from = Some(Zone::Graveyard);
        *to = Some(Zone::Battlefield);
    }
    first.sub_ability = Some(Box::new(second));
    let mut branch = departure_branch_reader();
    let Effect::ChooseOneOf { branches, .. } = branch.effect.as_mut() else {
        unreachable!()
    };
    branches[1] = first;
    root.sub_ability = Some(Box::new(branch));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental canonical Aura mass-return process", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut runner = scenario.build();
    let mut setup = vec![];
    for aura in &auras {
        engine::game::zones::move_to_zone(runner.state_mut(), *aura, Zone::Graveyard, &mut setup);
    }
    let original: Vec<_> = all
        .iter()
        .map(|id| {
            engine::types::identifiers::ObjectIdentityBinding::new(
                engine::types::identifiers::ObjectIncarnationRef::from_object(
                    &runner.state().objects[id],
                ),
                Zone::Graveyard,
            )
        })
        .collect();
    runner.cast(spell).commit();
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    let mut answered_auras = Vec::new();
    for _ in &auras {
        let WaitingFor::ReturnAsAuraTarget {
            player,
            returned_id,
            legal_targets,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("each actual Aura pauses the mass return before the remaining members");
        };
        assert_eq!(*player, P0);
        let aura = *returned_id;
        assert!(auras.contains(&aura));
        assert!(
            !answered_auras.contains(&aura),
            "each Aura pauses exactly once"
        );
        answered_auras.push(aura);
        assert!(legal_targets.contains(&TargetRef::Object(hosts[1])));
        let pending = runner
            .state()
            .active_change_zone_frame()
            .unwrap()
            .pending
            .as_ref()
            .unwrap();
        let (occurrence, result) = pending.pending_return_result_producer.unwrap();
        let group = &pending.logical_zone_change_group;
        let deliveries = group.instruction_deliveries.as_ref().unwrap();
        let group_id = group.logical_group_id;
        assert_eq!(deliveries.len(), 5);
        departure_assert_original_bindings(
            deliveries.iter().map(|delivery| delivery.binding),
            &original,
        );
        assert_eq!(result, ReturnResultId(7));
        assert!(pending.return_result_process.is_some());
        assert_eq!(
            runner.state().return_result_frames[&occurrence].process,
            pending.return_result_process
        );
        assert!(!runner.state().return_result_frames[&occurrence]
            .results
            .contains_key(&result));
        let mut hostile = runner.state().clone();
        hostile.return_result_frames.remove(&occurrence);
        hostile
            .return_result_frames
            .insert(foreign_o, foreign_frame.clone());
        hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
        hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
        let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
        departure_rewrite_lookup(
            &mut stack,
            &serde_json::to_value(occurrence).unwrap(),
            &serde_json::to_value(foreign_o).unwrap(),
        );
        hostile.resolution_stack = serde_json::from_value(stack).unwrap();
        hostile
            .active_change_zone_frame_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .pending_return_result_producer = Some((foreign_o, result));
        let saved = serde_json::to_value(&hostile).unwrap();
        let error = serde_json::from_value::<GameState>(saved)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("instruction-result process owner mismatch"),
            "saved mass-return publisher: {error}"
        );
        let mut hostile = engine::game::scenario::GameRunner::from_state(hostile);
        let before = serde_json::to_value(hostile.state()).unwrap();
        let error = hostile
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(hosts[1])),
            })
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("instruction-result process owner mismatch"),
            "public Aura host choice: {error}"
        );
        assert_eq!(serde_json::to_value(hostile.state()).unwrap(), before);
        for fault in 0..6 {
            let mut hostile = runner.state().clone();
            let foreign_binding = engine::types::identifiers::ObjectIdentityBinding::new(
                engine::types::identifiers::ObjectIncarnationRef::from_object(
                    &hostile.objects[&hosts[0]],
                ),
                Zone::Battlefield,
            );
            let group = &mut hostile
                .active_change_zone_frame_mut()
                .unwrap()
                .pending
                .as_mut()
                .unwrap()
                .logical_zone_change_group;
            let deliveries = group.instruction_deliveries.as_mut().unwrap();
            let current = deliveries
                .iter()
                .position(|delivery| delivery.binding.reference.object_id == aura)
                .unwrap();
            match fault {
                0 => deliveries.push(deliveries[0].clone()),
                1 => {
                    deliveries.remove(current);
                }
                2 => deliveries[current].binding.expected_zone = Zone::Exile,
                3 => {
                    deliveries[current].completion =
                        Some(engine::types::game_state::ZoneMoveCompletion::Moved)
                }
                4 => deliveries[current].binding = foreign_binding,
                5 => deliveries[current].binding.reference.incarnation += 1,
                _ => unreachable!(),
            }
            assert!(
                serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap())
                    .is_err()
            );
            assert!(
                serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                    serde_json::to_value(
                        engine::types::resolution::ResolutionStateWire::from_game_state(
                            hostile.clone()
                        )
                    )
                    .unwrap()
                )
                .is_err()
            );
            let mut hostile = GameRunner::from_state(hostile);
            let before = serde_json::to_value(hostile.state()).unwrap();
            let error = hostile
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(hosts[1])),
                })
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("logical") || error.contains("instruction"),
                "corrupt primary receipt {fault}: {error}"
            );
            assert_eq!(
                serde_json::to_value(hostile.state()).unwrap(),
                before,
                "preflight refuses before attaching an Aura or advancing its primary tail"
            );
        }
        // A GY tail of bare IDs cannot establish its original incarnations.
        // Compatibility must reject this active publisher rather than guess.
        let mut legacy = serde_json::to_value(runner.state()).unwrap();
        assert_eq!(departure_remove_instruction_deliveries(&mut legacy), 1);
        let unchanged = legacy.clone();
        let error = serde_json::from_value::<GameState>(legacy.clone())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("lacks original identity/primary completion authority"),
            "ambiguous GY publisher: {error}"
        );
        assert!(error.contains(&format!("{group_id:?}")));
        assert_eq!(legacy, unchanged);
        let mut legacy_wire = serde_json::to_value(
            engine::types::resolution::ResolutionStateWire::from_game_state(runner.state().clone()),
        )
        .unwrap();
        assert!(departure_remove_instruction_deliveries(&mut legacy_wire) > 0);
        let unchanged_wire = legacy_wire.clone();
        let error = serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
            legacy_wire.clone(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("lacks original identity/primary completion authority")
                && error.contains(&format!("{group_id:?}")),
            "ambiguous wire GY publisher: {error}"
        );
        assert_eq!(legacy_wire, unchanged_wire);
        departure_assert_private_result_views(runner.state());
        *runner.state_mut() = departure_restore_mass_owner(runner.state(), restore);
        departure_assert_original_bindings(
            runner
                .state()
                .active_change_zone_frame()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .logical_zone_change_group
                .instruction_deliveries
                .as_ref()
                .unwrap()
                .iter()
                .map(|delivery| delivery.binding),
            &original,
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(hosts[1])),
            })
            .unwrap();
        assert_eq!(
            runner.state().objects[&aura]
                .attached_to
                .and_then(|target| target.as_object()),
            Some(hosts[1])
        );
    }
    for card in &all {
        assert_eq!(runner.state().objects[card].zone, Zone::Battlefield);
    }
    assert!(runner.state().active_change_zone_frame().is_none());
    let frame = runner.state().return_result_frames.values().next().unwrap();
    let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
        unreachable!()
    };
    assert_eq!(records.len(), 5);
    departure_assert_original_bindings(
        records.iter().map(|record| {
            record
                .trigger_source_context
                .as_ref()
                .expect("every return record certifies its original graveyard incarnation")
                .identity
        }),
        &original,
    );
    let mut recorded: Vec<_> = records.iter().map(|record| record.object_id).collect();
    let mut expected = all;
    recorded.sort();
    expected.sort();
    assert_eq!(recorded, expected);
    assert!(records
        .iter()
        .all(|record| record.from_zone == Some(Zone::Graveyard)
            && record.to_zone == Zone::Battlefield));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    departure_assert_program_complete(&runner, spell, 5, 2);
}

#[test]
fn departure_per_player_zone_order_and_both_actual_pool_reads_survive_public_repauses() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{WaitingFor, ZoneOpponentChooserPurpose};
    let family = DepartureRetainedFamily::ZoneEach;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_retained_family_runner(family, false, empty);
        let WaitingFor::ChooseFromZoneOpponentChooser {
            candidates,
            purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("controller must publicly choose whose pool to choose from first");
        };
        assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
        runner
            .act(GameAction::ChooseZoneOpponentChooser {
                opponent: PlayerId(2),
            })
            .unwrap();
        for owner in [PlayerId(2), PlayerId(1)] {
            let WaitingFor::ChooseFromZoneChoice { player, cards, .. } =
                &runner.state().waiting_for
            else {
                panic!("each selected owner's actual addressed pool is offered");
            };
            assert_eq!(*player, PlayerId(0));
            assert_eq!(cards.len(), 2);
            assert!(cards
                .iter()
                .all(|id| subjects.contains(id) && runner.state().objects[id].owner == owner));
            let pending = runner.state().active_per_player_zone_choice().unwrap();
            assert_eq!(pending.current, Some(owner));
            assert_eq!(
                runner.state().return_result_frames[&pending.return_result_occurrence.unwrap()]
                    .process,
                pending.ability.return_result_process
            );
            if !empty {
                departure_assert_retained_hostile_reads(
                    &runner,
                    family,
                    foreign_o,
                    foreign_frame.clone(),
                );
            }
            let action = departure_retained_public_action(runner.state());
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect("actual per-player cursor, selected order and bound pool filter survive every re-pause");
            runner.act(action).unwrap();
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

fn departure_entry_member_action(state: &GameState) -> engine::types::actions::GameAction {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    match &state.waiting_for {
        WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 1 },
        WaitingFor::ReturnAsAuraTarget { legal_targets, .. } => {
            assert_eq!(legal_targets.len(), 2);
            GameAction::ChooseTarget {
                target: Some(legal_targets[1].clone()),
            }
        }
        other => panic!("real conditional-entry member pause: {other:?}"),
    }
}

fn departure_assert_entry_member_hostile(
    runner: &engine::game::scenario::GameRunner,
    foreign_o: engine::types::game_state::ReturnResultOccurrenceId,
    foreign_frame: &engine::types::game_state::ReturnResultFrame,
) {
    use engine::types::ability::ReturnResultId;
    use engine::types::game_state::{InstructionResult, RevealedInstructionResult};
    let pending = runner
        .state()
        .active_change_zone_frame()
        .unwrap()
        .pending
        .as_ref()
        .unwrap();
    let original = pending.return_result_occurrence.unwrap();
    let actual_p = pending.return_result_process.unwrap();
    assert_eq!(
        runner.state().return_result_frames[&original].process,
        Some(actual_p)
    );
    let action = departure_entry_member_action(runner.state());
    for fault in 0..7 {
        let mut hostile = runner.state().clone();
        match fault {
            0 => {
                hostile.return_result_frames.remove(&original);
                hostile
                    .return_result_frames
                    .insert(foreign_o, foreign_frame.clone());
                hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                departure_rewrite_lookup(
                    &mut stack,
                    &serde_json::to_value(original).unwrap(),
                    &serde_json::to_value(foreign_o).unwrap(),
                );
                hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                assert!(!hostile.return_result_frames.contains_key(&original));
                assert_eq!(
                    hostile
                        .active_change_zone_frame()
                        .unwrap()
                        .pending
                        .as_ref()
                        .unwrap()
                        .return_result_process,
                    Some(actual_p)
                );
            }
            1 => hostile.return_result_frames.clear(),
            2 => {
                hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .remove(&ReturnResultId(7));
            }
            3 => {
                hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .insert(
                        ReturnResultId(7),
                        InstructionResult::RevealedCards(RevealedInstructionResult {
                            revealed_cards: vec![],
                        }),
                    );
            }
            4 => {
                hostile
                    .active_change_zone_frame_mut()
                    .unwrap()
                    .pending
                    .as_mut()
                    .unwrap()
                    .return_result_process = None
            }
            5 => {
                hostile
                    .active_change_zone_frame_mut()
                    .unwrap()
                    .pending
                    .as_mut()
                    .unwrap()
                    .return_result_occurrence = None
            }
            6 => {
                let InstructionResult::ZoneChanges(records) = hostile
                    .return_result_frames
                    .get_mut(&original)
                    .unwrap()
                    .results
                    .get_mut(&ReturnResultId(7))
                    .unwrap()
                else {
                    unreachable!()
                };
                records[0].trigger_source_context = None;
            }
            _ => unreachable!(),
        }
        let error = serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap())
            .unwrap_err()
            .to_string();
        if fault == 0 || fault == 4 {
            assert!(
                error.contains("instruction-result process owner mismatch"),
                "saved actual member: {error}"
            );
        }
        let mut reached = engine::game::scenario::GameRunner::from_state(hostile);
        let before = serde_json::to_value(reached.state()).unwrap();
        let error = reached.act(action.clone()).unwrap_err().to_string();
        if fault == 0 || fault == 4 {
            assert!(
                error.contains("instruction-result process owner mismatch"),
                "public actual member: {error}"
            );
        }
        assert_eq!(serde_json::to_value(reached.state()).unwrap(), before, "invalid member work cannot attach an Aura, deliver another member, or alter counters/events/selection");
    }
    for viewer in [None, Some(PlayerId(0)), Some(PlayerId(1))] {
        let projected = match viewer {
            Some(player) => {
                engine::game::visibility::filter_state_for_viewer(runner.state(), player)
            }
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        };
        assert!(projected.return_result_frames.is_empty());
        assert!(projected.resolution_stack.is_empty());
        assert!(!serde_json::to_string(&projected)
            .unwrap()
            .contains("return_result_process"));
    }
}

#[test]
fn departure_conditional_entry_count_predicate_gate_members_aura_and_repauses_use_saved_process() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for members in [1, 2] {
        let (mut foreign, foreign_spell, foreign_members) =
            departure_zone_entry_prompt_with_members(true, true, false, false, false, members);
        let (&foreign_o, foreign_frame) =
            foreign.state().return_result_frames.iter().next().unwrap();
        let foreign_frame = foreign_frame.clone();
        let foreign_p = foreign_frame.process.unwrap();
        foreign
            .act(GameAction::SelectCards {
                cards: foreign_members.to_vec(),
            })
            .unwrap();
        for _ in 0..2 {
            let action = departure_entry_member_action(foreign.state());
            foreign.act(action).unwrap();
        }
        assert!(foreign.state().return_result_frames.is_empty());
        assert!(foreign.state().resolution_stack.is_empty());
        assert_eq!(
            foreign.state().objects[&foreign_spell].zone,
            Zone::Graveyard
        );
        assert_eq!(
            foreign.state().objects[&foreign_members[1]]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(7)
        );
        for empty in [false, true] {
            for (predicate, gate) in [(false, false), (true, false), (false, true)] {
                let (mut runner, spell, selected) = departure_zone_entry_prompt_with_members(
                    true, false, empty, predicate, gate, members,
                );
                let WaitingFor::EffectZoneChoice {
                    count,
                    min_count,
                    return_result_process: Some(expected),
                    ..
                } = runner.state().waiting_for
                else {
                    unreachable!()
                };
                assert_eq!((count, min_count), (2, 2));
                runner.state_mut().next_return_result_process_id = foreign_p.0 + 1;
                runner
                    .state_mut()
                    .resolving_stack_entry
                    .as_mut()
                    .unwrap()
                    .ability_mut()
                    .unwrap()
                    .return_result_process = Some(foreign_p);
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect("initial multi-member prompt uses its own P even when attribution has another root P");
                runner
                    .act(GameAction::SelectCards {
                        cards: selected.to_vec(),
                    })
                    .unwrap();
                for index in 0..2 {
                    let pending = runner
                        .state()
                        .active_change_zone_frame()
                        .unwrap()
                        .pending
                        .as_ref()
                        .unwrap();
                    assert_eq!(pending.return_result_process, Some(expected));
                    assert_ne!(pending.return_result_process, Some(foreign_p));
                    if !empty {
                        departure_assert_entry_member_hostile(&runner, foreign_o, &foreign_frame);
                    }
                    assert_eq!(
                        runner.state().objects[&selected[1]].zone,
                        Zone::Graveyard,
                        "the remaining member cannot move before the first pause is answered"
                    );
                    let action = departure_entry_member_action(runner.state());
                    let saved = serde_json::to_value(runner.state()).unwrap();
                    *runner.state_mut() = serde_json::from_value(saved).expect("every actual member/Aura replacement re-pause preserves the same explicit P and unresolved dependency walk");
                    runner.act(action).unwrap();
                    if index == 0 {
                        assert!(runner.state().active_change_zone_frame().is_some());
                    }
                }
                for id in selected {
                    assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
                    assert_eq!(
                        runner.state().objects[&id]
                            .counters
                            .get(&CounterType::Plus1Plus1)
                            .copied()
                            .unwrap_or(0),
                        if empty { 0 } else { 5 }
                    );
                    assert_eq!(runner.state().objects[&id].tapped, gate && !empty);
                }
                assert_eq!(
                    runner.state().objects[&selected[1]].power,
                    Some(if empty { 2 } else { 7 })
                );
                assert_eq!(
                    runner.state().objects[&selected[1]].toughness,
                    Some(if empty { 2 } else { 7 })
                );
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(runner.state().stack.is_empty());
                assert!(runner.state().resolution_stack.is_empty());
                assert!(runner.state().return_result_frames.is_empty());
            }
        }
    }
}

#[test]
fn departure_single_candidate_entry_reads_the_actual_bound_ability_without_parking() {
    use engine::game::scenario::P0;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, Effect, FilterProp, TargetChoiceTiming,
        TargetFilter, TypedFilter,
    };
    for empty in [false, true] {
        for (predicate, gate) in [(false, false), (true, false), (false, true)] {
            let (runner, spell, candidates) = departure_public_program_runner(
                false,
                empty,
                2,
                |scenario| {
                    let bear = scenario
                        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
                        .with_mana_cost(engine::types::mana::ManaCost::Cost {
                            generic: 1,
                            shards: vec![engine::types::mana::ManaCostShard::Green],
                        })
                        .id();
                    let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                    else {
                        unreachable!()
                    };
                    let mut threshold = TypedFilter::default();
                    threshold.properties.push(FilterProp::Cmc {
                        comparator: Comparator::LT,
                        value: amount.clone(),
                    });
                    let threshold = TargetFilter::Typed(threshold);
                    let mut effect: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZone", "origin":"Graveyard", "destination":"Battlefield"})).unwrap();
                    let Effect::ChangeZone {
                        target,
                        conditional_enter_with_counters,
                        enters_modified_if,
                        enter_tapped,
                        ..
                    } = &mut effect
                    else {
                        unreachable!()
                    };
                    *target = TargetFilter::Typed(TypedFilter::creature());
                    conditional_enter_with_counters.push((
                        if predicate {
                            threshold.clone()
                        } else {
                            TargetFilter::Any
                        },
                        CounterType::Plus1Plus1,
                        amount,
                    ));
                    if gate {
                        *enters_modified_if = Some(threshold);
                        *enter_tapped = engine::types::zones::EtbTapState::Tapped;
                    }
                    let mut entry = AbilityDefinition::new(AbilityKind::Spell, effect);
                    entry.target_choice_timing = TargetChoiceTiming::Resolution;
                    (entry, vec![bear])
                },
            );
            let bear = candidates[0];
            assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().objects[&bear]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                if empty { 0 } else { 5 }
            );
            assert_eq!(
                runner.state().objects[&bear].power,
                Some(if empty { 2 } else { 7 })
            );
            assert_eq!(
                runner.state().objects[&bear].toughness,
                Some(if empty { 2 } else { 7 })
            );
            assert_eq!(runner.state().objects[&bear].tapped, gate && !empty);
            departure_assert_program_complete(&runner, spell, 0, 0);
        }
    }
}

fn departure_clear_continuation_process(value: &mut serde_json::Value) -> usize {
    let mut changed = 0;
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(serde_json::Value::Object(chain)) = fields.get_mut("chain") {
                if chain
                    .get("return_result_process")
                    .is_some_and(|value| !value.is_null())
                {
                    chain.insert("return_result_process".into(), serde_json::Value::Null);
                    changed += 1;
                }
            }
            for child in fields.values_mut() {
                changed += departure_clear_continuation_process(child);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                changed += departure_clear_continuation_process(child);
            }
        }
        _ => {}
    }
    changed
}

#[test]
fn departure_latched_discard_and_sacrifice_keep_the_narrow_tail_owner_until_the_public_answer() {
    use engine::types::game_state::WaitingFor;
    for family in [
        DepartureRetainedFamily::DiscardTail,
        DepartureRetainedFamily::SacrificeTail,
    ] {
        let (mut runner, spell, subjects) = departure_retained_family_runner(family, false, false);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { .. } | WaitingFor::EffectZoneChoice { .. }
        ));
        assert!(
            !serde_json::to_string(&runner.state().waiting_for)
                .unwrap()
                .contains("return_result_process"),
            "a latched selection does not acquire a fictitious reader identity"
        );
        let pending = runner
            .state()
            .resolution_stack
            .iter()
            .find_map(|frame| match frame {
                engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                    Some(&frame.pending)
                }
                _ => None,
            })
            .unwrap();
        let occurrence = pending.return_result_occurrence.unwrap();
        assert_eq!(
            runner.state().return_result_frames[&occurrence].process,
            pending.chain.return_result_process
        );
        let action = departure_retained_public_action(runner.state());
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect(
            "the latched prompt and its distinct actual narrow continuation survive reload",
        );
        runner.act(action).unwrap();
        assert_eq!(
            subjects
                .iter()
                .filter(|id| runner.state().objects[id].zone == Zone::Graveyard)
                .count(),
            2
        );
        departure_assert_program_complete(&runner, spell, 5, 2);
    }
}

fn departure_drain_until_loss(runner: &mut engine::game::scenario::GameRunner) -> i32 {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let mut wins = 0;
    for index in 0..32 {
        let WaitingFor::CoinFlipKeepChoice {
            results,
            keep_count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("every logical until-loss flip reaches its actual Thumb keep choice");
        };
        assert_eq!(results.len(), 2);
        assert_eq!(*keep_count, 1);
        let keep = if index == 0 {
            results
                .iter()
                .position(|result| *result)
                .expect("the pinned deterministic first pair supplies a nonzero win witness")
        } else {
            results.iter().position(|result| !*result).unwrap_or(1)
        };
        let won = results[keep];
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("every until-loss re-pause retains the exact imported branch process");
        runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![keep],
            })
            .unwrap();
        if won {
            wins += 1;
        } else {
            assert!(wins > 0);
            return wins;
        }
    }
    panic!("the deterministic until-loss fixture must terminate");
}

#[test]
fn departure_until_loss_repauses_reload_imports_each_winning_body_and_retires_after_the_loss() {
    let family = DepartureRetainedFamily::CoinUntil;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    let wins = departure_drain_until_loss(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7 * wins, 2 * wins);
    let (positive, spell, _) = departure_retained_family_runner(family, false, false);
    departure_assert_retained_hostile_reads(&positive, family, foreign_o, foreign_frame);
    let mut positive = positive;
    let wins = departure_drain_until_loss(&mut positive);
    departure_assert_program_complete(&positive, spell, 5 * wins, 2 * wins);
    let (mut empty, empty_spell, _) = departure_retained_family_runner(family, false, true);
    assert!(departure_drain_until_loss(&mut empty) > 0);
    departure_assert_program_complete(&empty, empty_spell, 0, 0);
}

#[test]
fn departure_whole_producing_repeat_mints_a_new_process_for_the_completed_empty_second_iteration() {
    use engine::types::ability::{QuantityExpr, ReturnResultId};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    let (mut runner, spell, _) = departure_public_program_runner_with_setup(
        false,
        false,
        2,
        |_| (departure_branch_reader(), vec![]),
        |state, _| {
            let source = state
                .objects
                .values()
                .find(|object| object.name == "Supplemental retained public program")
                .unwrap()
                .id;
            let object = state.objects.get_mut(&source).unwrap();
            std::sync::Arc::make_mut(&mut object.abilities)[0].repeat_for =
                Some(QuantityExpr::Fixed { value: 2 });
            object.base_abilities = object.abilities.clone();
        },
    );
    let mut previous = None;
    for iteration in 0..2 {
        let WaitingFor::ChooseOneOfBranch {
            return_result_occurrence: Some(occurrence),
            expected_return_result_process: Some(process),
            ..
        } = runner.state().waiting_for
        else {
            panic!("each whole producing iteration enters its actual public selected reader");
        };
        let frame = &runner.state().return_result_frames[&occurrence];
        assert_eq!(frame.process, Some(process));
        let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
            unreachable!()
        };
        assert_eq!(records.len(), if iteration == 0 { 3 } else { 0 });
        if let Some((old_o, old_p)) = previous {
            assert_ne!(old_o, occurrence);
            assert_ne!(old_p, process);
            assert!(
                !runner.state().return_result_frames.contains_key(&old_o),
                "the next whole process cannot read the retired previous iteration"
            );
        }
        previous = Some((occurrence, process));
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect(
            "the currently bound producing iteration, including its genuine empty result, reloads",
        );
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        assert_eq!(runner.state().players[0].life, 25);
        assert_eq!(runner.state().players[1].life, 22);
    }
    departure_assert_program_complete(&runner, spell, 5, 2);
}

#[test]
fn departure_read_only_repeat_imports_the_same_completed_five_twice_without_allocating_empty_frames(
) {
    use engine::types::ability::{QuantityExpr, ReturnResultId};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    let (mut runner, spell, _) = departure_public_program_runner(false, false, 2, |_| {
        let mut choice = departure_branch_reader();
        choice.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
        (choice, vec![])
    });
    let mut original = None;
    for iteration in 0..2 {
        let WaitingFor::ChooseOneOfBranch {
            return_result_occurrence: Some(occurrence),
            expected_return_result_process: Some(process),
            ..
        } = runner.state().waiting_for
        else {
            panic!("read-only repeated selected body really re-pauses");
        };
        assert_eq!(runner.state().return_result_frames.len(), 1);
        assert_eq!(
            runner.state().return_result_frames[&occurrence].process,
            Some(process)
        );
        assert!(
            matches!(&runner.state().return_result_frames[&occurrence].results[&ReturnResultId(7)], InstructionResult::ZoneChanges(records) if records.len() == 3)
        );
        if let Some(original) = original {
            assert_eq!((occurrence, process), original);
        } else {
            original = Some((occurrence, process));
        }
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the imported read-only repeated body keeps its actual outer owner");
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        assert_eq!(runner.state().players[0].life, 20 + 5 * (iteration + 1));
        assert_eq!(runner.state().players[1].life, 20 + 2 * (iteration + 1));
    }
    departure_assert_program_complete(&runner, spell, 10, 4);
}

#[test]
fn departure_repeated_complete_local_child_preserves_outer_five_while_its_three_then_empty_processes_retire(
) {
    use engine::game::scenario::P1;
    use engine::types::ability::{Effect, QuantityExpr, ReturnResultId, TargetFilter};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    let (mut runner, spell, subjects) =
        departure_public_program_runner(false, false, 2, |scenario| {
            let inner = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
            let mut local = departure_exile_definition(11);
            let Effect::ChangeZoneAll { target, .. } = local.effect.as_mut() else {
                unreachable!()
            };
            *target = TargetFilter::SpecificObject { id: inner };
            let mut inner_choice = departure_branch_reader();
            let Effect::ChooseOneOf { branches, .. } = inner_choice.effect.as_mut() else {
                unreachable!()
            };
            branches[1] = departure_gain_definition(11, P1);
            local.sub_ability = Some(Box::new(inner_choice));
            local.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
            let mut outer = departure_branch_reader();
            let Effect::ChooseOneOf { branches, .. } = outer.effect.as_mut() else {
                unreachable!()
            };
            branches[1] = local;
            outer.sub_ability = Some(Box::new(departure_dual_reader()));
            (outer, vec![inner])
        });
    let (&outer_o, outer_frame) = runner.state().return_result_frames.iter().next().unwrap();
    let outer_p = outer_frame.process.unwrap();
    assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Battlefield);
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    let mut previous = None;
    for iteration in 0..2 {
        let WaitingFor::ChooseOneOfBranch {
            return_result_occurrence: Some(occurrence),
            expected_return_result_process: Some(process),
            ..
        } = runner.state().waiting_for
        else {
            panic!("the complete local child has its own selected reader at each iteration");
        };
        assert_ne!(occurrence, outer_o);
        assert_ne!(process, outer_p);
        assert_eq!(runner.state().return_result_frames.len(), 2);
        assert_eq!(
            runner.state().return_result_frames[&outer_o].process,
            Some(outer_p)
        );
        let InstructionResult::ZoneChanges(records) =
            &runner.state().return_result_frames[&occurrence].results[&ReturnResultId(11)]
        else {
            unreachable!()
        };
        assert_eq!(records.len(), if iteration == 0 { 1 } else { 0 });
        if iteration == 0 {
            assert_eq!(records[0].power, Some(3));
        }
        if let Some((old_o, old_p)) = previous {
            assert_ne!(old_o, occurrence);
            assert_ne!(old_p, process);
            assert!(!runner.state().return_result_frames.contains_key(&old_o));
        }
        previous = Some((occurrence, process));
        assert_eq!(
            runner.state().players[0].life,
            20,
            "the independent outer tail stays outside both local iterations"
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect(
            "both the local current process and distinct outer import survive each child re-pause",
        );
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        if iteration == 0 {
            assert_eq!(runner.state().players[1].life, 23);
        }
    }
    assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Exile);
    departure_assert_program_complete(&runner, spell, 5, 5);
}

#[test]
fn departure_optional_multi_target_public_filter_and_selected_body_keep_their_actual_owner() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let family = DepartureRetainedFamily::MultiTarget;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_retained_family_runner(family, false, empty);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OpponentMayChoice {
                player: PlayerId(1),
                ..
            }
        ));
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the optional frame keeps the actual bound eligibility filter");
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::MultiTargetSelection {
            player,
            legal_targets,
            min_targets,
            max_targets,
            pending_ability,
            return_result_occurrence: Some(occurrence),
        } = &runner.state().waiting_for
        else {
            panic!(
                "accepting the real opponent optional effect enters its actual target selection"
            );
        };
        assert_eq!(*player, PlayerId(1));
        assert_eq!(legal_targets, &subjects);
        assert_eq!((*min_targets, *max_targets), (1, 1));
        assert_eq!(
            runner.state().return_result_frames[occurrence].process,
            pending_ability.return_result_process
        );
        assert!(
            pending_ability.sub_ability.is_none(),
            "the independently retained narrow tail is outside the selected pending executable"
        );
        if !empty {
            departure_assert_retained_hostile_reads(
                &runner,
                family,
                foreign_o,
                foreign_frame.clone(),
            );
        }
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner
            .act(GameAction::SelectTargets {
                targets: vec![
                    engine::types::ability::TargetRef::Object(subjects[1]),
                    engine::types::ability::TargetRef::Object(subjects[1])
                ]
            })
            .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the actual optional-resolution target owner survives reload");
        runner
            .act(GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Object(subjects[1])],
            })
            .unwrap();
        assert!(!runner.state().objects[&subjects[0]].tapped);
        assert!(runner.state().objects[&subjects[1]].tapped);
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

#[test]
fn departure_empty_reciprocal_producer_binds_only_its_actual_imported_consumer_before_public_selection(
) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{WaitingFor, ZoneOpponentChooserPurpose};
    let family = DepartureRetainedFamily::ZoneReciprocal;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_retained_family_runner(family, false, empty);
        let WaitingFor::ChooseFromZoneOpponentChooser {
            purpose: ZoneOpponentChooserPurpose::BindReciprocalConsume,
            ability,
            return_result_occurrence: Some(occurrence),
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the genuinely empty reciprocal producer requires the actual consumer-binding election");
        };
        let owner = runner.state().active_ability_continuation().unwrap();
        assert_eq!(owner.return_result_occurrence, Some(*occurrence));
        assert_eq!(
            owner.chain.return_result_process,
            ability.return_result_process
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("binding prompt and exact active consumer agree after reload");
        runner
            .act(GameAction::ChooseZoneOpponentChooser {
                opponent: PlayerId(2),
            })
            .unwrap();
        let WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            reciprocal_role,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the elected opponent actually receives the pending consumer pool");
        };
        assert_eq!(*player, PlayerId(2));
        assert_eq!(cards, &subjects);
        assert!(matches!(
            reciprocal_role,
            Some(engine::types::ability::ReciprocalZoneChoiceRole::Consume)
        ));
        if !empty {
            departure_assert_retained_hostile_reads(
                &runner,
                family,
                foreign_o,
                foreign_frame.clone(),
            );
        }
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the bound selected consumer and narrow tail survive the second public pause");
        runner
            .act(GameAction::SelectCards {
                cards: vec![subjects[1]],
            })
            .unwrap();
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

fn departure_finish_substitute_zone_choices(
    runner: &mut engine::game::scenario::GameRunner,
    hostile: Option<(
        engine::types::game_state::ReturnResultOccurrenceId,
        engine::types::game_state::ReturnResultFrame,
    )>,
) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{WaitingFor, ZoneOpponentChooserPurpose};
    let WaitingFor::ChooseFromZoneOpponentChooser {
        player,
        purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("the living next player receives the deceased controller's order choice");
    };
    assert_eq!(*player, PlayerId(1));
    assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
    runner
        .act(GameAction::ChooseZoneOpponentChooser {
            opponent: PlayerId(2),
        })
        .unwrap();
    for owner in [PlayerId(2), PlayerId(1)] {
        let WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            purpose: ZoneOpponentChooserPurpose::SubstituteChooser,
            candidates,
            ability,
            return_result_occurrence: Some(occurrence),
        } = &runner.state().waiting_for
        else {
            panic!("each pending pool requires its actual substitute-chooser election");
        };
        assert_eq!(*player, PlayerId(1));
        assert_eq!(candidates, &[PlayerId(1), PlayerId(2)]);
        let pending = runner.state().active_per_player_zone_choice().unwrap();
        assert_eq!(pending.current, Some(owner));
        assert_eq!(pending.nominee, None);
        assert_eq!(pending.return_result_occurrence, Some(*occurrence));
        assert_eq!(
            pending.ability.return_result_process,
            ability.return_result_process
        );
        if let Some((foreign_o, foreign_frame)) = &hostile {
            departure_assert_retained_hostile_reads(
                runner,
                DepartureRetainedFamily::ZoneEach,
                *foreign_o,
                foreign_frame.clone(),
            );
        }
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect("the real substitute election retains independent body authority after the controller leaves");
        runner
            .act(GameAction::ChooseZoneOpponentChooser {
                opponent: PlayerId(2),
            })
            .unwrap();
        let WaitingFor::ChooseFromZoneChoice { player, cards, .. } = &runner.state().waiting_for
        else {
            panic!("the elected substitute actually makes this pending pool choice");
        };
        assert_eq!(*player, PlayerId(2));
        assert_eq!(cards.len(), 2);
        assert!(cards
            .iter()
            .all(|id| runner.state().objects[id].owner == owner));
        assert_eq!(
            runner
                .state()
                .active_per_player_zone_choice()
                .unwrap()
                .nominee,
            Some(PlayerId(2))
        );
        if let Some((foreign_o, foreign_frame)) = &hostile {
            departure_assert_retained_hostile_reads(
                runner,
                DepartureRetainedFamily::ZoneEach,
                *foreign_o,
                foreign_frame.clone(),
            );
        }
        let action = departure_retained_public_action(runner.state());
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the exact elected maker and pending owner survive the card-choice re-pause");
        runner.act(action).unwrap();
    }
}

#[test]
fn departure_substitute_chooser_keeps_captured_five_after_actual_controller_departure() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let family = DepartureRetainedFamily::ZoneEach;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    foreign
        .act(GameAction::Concede {
            player_id: PlayerId(0),
        })
        .unwrap();
    departure_finish_substitute_zone_choices(&mut foreign, None);
    assert!(!foreign.state().objects.contains_key(&foreign_spell));
    assert_eq!(foreign.state().players[1].life, 22);
    assert!(foreign.state().return_result_frames.is_empty());
    assert!(foreign.state().resolution_stack.is_empty());
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_retained_family_runner(family, false, empty);
        runner
            .act(GameAction::Concede {
                player_id: PlayerId(0),
            })
            .unwrap();
        assert!(runner.state().players[0].is_eliminated);
        departure_finish_substitute_zone_choices(
            &mut runner,
            if empty {
                None
            } else {
                Some((foreign_o, foreign_frame.clone()))
            },
        );
        assert_eq!(runner.state().players[1].life, if empty { 20 } else { 22 });
        assert!(!runner.state().objects.contains_key(&spell), "the departing player's owned resolving source leaves the game while its already-started resolution completes");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player != PlayerId(0))
        );
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
    }
}

#[test]
fn departure_category_counter_public_members_use_saved_process_without_an_outer_tail() {
    use engine::types::game_state::WaitingFor;
    let family = DepartureRetainedFamily::CategoryCounter;
    let (mut foreign, foreign_spell, foreign_subjects) =
        departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 0, 0);
    assert_eq!(
        foreign.state().objects[&foreign_subjects[1]].counters[&CounterType::Plus1Plus1],
        7
    );
    assert_eq!(
        foreign.state().objects[&foreign_subjects[3]].counters[&CounterType::Plus1Plus1],
        7
    );
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_retained_family_runner(family, false, empty);
        for index in [1usize, 3] {
            let WaitingFor::ChooseFromZoneChoice { player, cards, .. } =
                &runner.state().waiting_for
            else {
                panic!("each nonempty category member really offers its two candidates");
            };
            assert_eq!(*player, PlayerId(0));
            assert_eq!(cards.len(), 2);
            assert_eq!(cards[1], subjects[index]);
            assert!(
                runner
                    .state()
                    .resolution_stack
                    .iter()
                    .find_map(|frame| match frame {
                        engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) =>
                            Some(&frame.pending),
                        _ => None,
                    })
                    .is_none(),
                "the final category itself owns the future unresolved counter read"
            );
            let pending = runner.state().active_per_category_zone_choice().unwrap();
            assert_eq!(
                runner.state().return_result_frames[&pending.return_result_occurrence.unwrap()]
                    .process,
                pending.ability.return_result_process
            );
            if !empty {
                departure_assert_retained_hostile_reads(
                    &runner,
                    family,
                    foreign_o,
                    foreign_frame.clone(),
                );
            }
            let action = departure_retained_public_action(runner.state());
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("each actual category counter owner and unresolved member filter reloads");
            runner.act(action).unwrap();
        }
        for (index, id) in subjects.iter().enumerate() {
            assert_eq!(
                runner.state().objects[id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                if index % 2 == 1 && !empty { 5 } else { 0 }
            );
        }
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
}

#[test]
fn departure_category_single_candidate_automatic_reads_use_the_actual_bound_count_view() {
    use engine::game::scenario::P0;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Chooser, Effect, ForEachCategoryAction, IterationCategory,
        TargetFilter, TypedFilter,
    };
    use engine::types::mana::ManaColor;
    for empty in [false, true] {
        let (runner, spell, subjects) =
            departure_public_program_runner(false, empty, 2, |scenario| {
                let subjects = vec![
                    scenario
                        .add_artifact_from_oracle(P0, "Supplemental single red category member", "")
                        .with_color(vec![ManaColor::Red])
                        .id(),
                    scenario
                        .add_artifact_from_oracle(
                            P0,
                            "Supplemental single green category member",
                            "",
                        )
                        .with_color(vec![ManaColor::Green])
                        .id(),
                ];
                let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                (
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ForEachCategory {
                            category: IterationCategory::Color,
                            chooser: Chooser::Controller,
                            action: ForEachCategoryAction::PutCounter {
                                target: TargetFilter::Typed(TypedFilter::new(
                                    engine::types::ability::TypeFilter::Artifact,
                                )),
                                counter_type: CounterType::Plus1Plus1,
                                count: amount,
                            },
                        },
                    ),
                    subjects,
                )
            });
        for id in subjects {
            assert_eq!(
                runner.state().objects[&id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                if empty { 0 } else { 5 }
            );
        }
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
}

#[test]
fn departure_category_exile_batch_completion_retains_actual_template_and_remaining_filter_authority(
) {
    use engine::types::ability::{Comparator, Effect, FilterProp, TargetFilter, TypedFilter};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{BatchCompletion, WaitingFor};
    let family = DepartureRetainedFamily::CategoryExile;
    let (mut foreign, foreign_spell, _) = departure_retained_family_runner(family, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_retained_family_runner(family, false, empty);
        for index in [1usize, 3] {
            let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for else {
                panic!("the next category member's actual public choice is offered");
            };
            assert_eq!(cards.len(), 2);
            assert_eq!(cards[1], subjects[index]);
            if !empty {
                departure_assert_retained_hostile_reads(
                    &runner,
                    family,
                    foreign_o,
                    foreign_frame.clone(),
                );
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() =
                serde_json::from_value(saved).expect("category member reloads before selection");
            runner
                .act(GameAction::SelectCards {
                    cards: vec![subjects[index]],
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            if index == 1 && !empty {
                let Effect::GainLife { amount, .. } =
                    *departure_gain_definition(7, PlayerId(0)).effect
                else {
                    unreachable!()
                };
                let mut predicate = TypedFilter::default();
                predicate.properties.push(FilterProp::Cmc {
                    comparator: Comparator::LT,
                    value: amount,
                });
                let BatchCompletion::ForEachCategoryExileComplete {
                    remaining_member_filters,
                    ..
                } = runner
                    .state_mut()
                    .active_batch_delivery_mut()
                    .unwrap()
                    .completion
                    .as_mut()
                    .unwrap()
                else {
                    unreachable!()
                };
                assert!(!remaining_member_filters.is_empty());
                // Supplemental canonical retained-filter fixture on an existing reached owner.
                // This asserts the typed owner contract, not synthesis of a printed clause.
                for filter in remaining_member_filters {
                    *filter = TargetFilter::And {
                        filters: vec![filter.clone(), TargetFilter::Typed(predicate.clone())],
                    };
                }
            }
            let batch = runner.state().active_batch_delivery().unwrap();
            let BatchCompletion::ForEachCategoryExileComplete {
                return_result_occurrence: Some(occurrence),
                ability,
                remaining_member_filters,
                ..
            } = batch.completion.as_ref().unwrap()
            else {
                panic!("actual exile batch carries the selected template and its own O");
            };
            assert_eq!(
                runner.state().return_result_frames[occurrence].process,
                ability.return_result_process
            );
            if index == 1 {
                assert!(!remaining_member_filters.is_empty());
            }
            assert_eq!(runner.state().players[0].life, 20);
            assert_eq!(runner.state().players[1].life, 20);
            if !empty {
                departure_assert_retained_hostile_reads(
                    &runner,
                    family,
                    foreign_o,
                    foreign_frame.clone(),
                );
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect("the actual category completion's template, independent P, O and remaining predicate survive replacement reload");
            runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap();
        }
        for (index, id) in subjects.iter().enumerate() {
            assert_eq!(
                runner.state().objects[id].zone,
                if index % 2 == 1 {
                    Zone::Exile
                } else {
                    Zone::Library
                }
            );
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

#[test]
fn departure_controller_repeat_decision_restores_import_before_the_next_real_branch() {
    use engine::types::ability::{RepeatContinuation, ReturnResultId};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    // Supplemental canonical composition: the paid producer is unchanged; the selected
    // read-only process can be repeated by its real public controller decision.
    let (mut foreign, foreign_spell, _) =
        departure_retained_family_runner(DepartureRetainedFamily::Branch, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    departure_drain_retained_public_program(&mut foreign);
    departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |_| {
            let mut body = departure_branch_reader();
            body.repeat_until = Some(RepeatContinuation::ControllerChoice);
            (body, vec![])
        });
        let mut authority = None;
        for iteration in 0..2 {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseOneOfBranch {
                    player: PlayerId(0),
                    ..
                }
            ));
            departure_assert_retained_hostile_reads(
                &runner,
                DepartureRetainedFamily::Branch,
                foreign_o,
                foreign_frame.clone(),
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("the selected iteration reloads before its live read");
            runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
            let WaitingFor::RepeatDecision {
                player,
                ability,
                return_result_occurrence: Some(occurrence),
            } = &runner.state().waiting_for
            else {
                panic!("the real repeat controller prompt retains the next executable");
            };
            assert_eq!(*player, PlayerId(0));
            departure_assert_private_result_views(runner.state());
            let occurrence = *occurrence;
            let process = ability.return_result_process.unwrap();
            assert_eq!(
                runner.state().return_result_frames[&occurrence].process,
                Some(process)
            );
            assert!(
                matches!(&runner.state().return_result_frames[&occurrence].results[&ReturnResultId(7)], InstructionResult::ZoneChanges(records) if records.len() == if empty { 0 } else { 3 })
            );
            if let Some(old) = authority {
                assert_eq!(old, (occurrence, process));
            } else {
                authority = Some((occurrence, process));
            }
            assert_eq!(
                runner.state().players[0].life,
                20 + if empty { 0 } else { 5 * (iteration + 1) }
            );
            for fault in 0..3 {
                let mut hostile = runner.state().clone();
                if fault == 0 {
                    hostile.return_result_frames.clear();
                    hostile
                        .return_result_frames
                        .insert(foreign_o, foreign_frame.clone());
                    hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                    hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                    let mut waiting = serde_json::to_value(&hostile.waiting_for).unwrap();
                    departure_rewrite_lookup(
                        &mut waiting,
                        &serde_json::json!(occurrence),
                        &serde_json::json!(foreign_o),
                    );
                    hostile.waiting_for = serde_json::from_value(waiting).unwrap();
                } else if let WaitingFor::RepeatDecision {
                    ability,
                    return_result_occurrence,
                    ..
                } = &mut hostile.waiting_for
                {
                    if fault == 1 {
                        ability.return_result_process = None;
                    } else {
                        *return_result_occurrence = None;
                    }
                }
                let wire = serde_json::to_value(&hostile).unwrap();
                assert!(
                    serde_json::from_value::<engine::types::game_state::GameState>(wire.clone())
                        .is_err()
                );
                let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
                let error = bad
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap_err();
                if fault < 2 {
                    assert!(format!("{error:?}").contains("process owner mismatch"));
                }
                assert_eq!(
                    serde_json::to_value(bad.state()).unwrap(),
                    wire,
                    "reject before resetting priority or running another iteration"
                );
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect(
                "the future iteration's actual O and independent P survive at RepeatDecision",
            );
            runner
                .act(GameAction::DecideOptionalEffect {
                    accept: iteration == 0,
                })
                .unwrap();
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 10 },
            if empty { 0 } else { 4 },
        );
    }
}

#[test]
fn departure_while_condition_rechecks_the_bound_import_after_each_public_child() {
    use engine::types::ability::{
        AbilityCondition, Comparator, Effect, QuantityExpr, RepeatContinuation,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |_| {
            let mut body = departure_branch_reader();
            let Effect::GainLife { amount, .. } = *departure_gain_definition(7, PlayerId(0)).effect
            else {
                unreachable!()
            };
            body.repeat_until = Some(RepeatContinuation::WhileCondition {
                condition: Box::new(AbilityCondition::QuantityCheck {
                    lhs: amount,
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }),
                max_iterations: Some(1),
            });
            (body, vec![])
        });
        let passes = if empty { 1 } else { 2 };
        let mut selected = None;
        for _ in 0..passes {
            let WaitingFor::ChooseOneOfBranch {
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(process),
                ..
            } = runner.state().waiting_for
            else {
                panic!("the bounded while process reaches its actual interactive child");
            };
            if let Some(old) = selected {
                assert_eq!(old, (occurrence, process));
            } else {
                selected = Some((occurrence, process));
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("both child and pending repeat condition retain their selected owner");
            runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 10 },
            if empty { 0 } else { 4 },
        );
    }
}

// Supplemental canonical resolution-payment compositions. The real paid producer
// remains the same five/two departure population; every cost move uses the normal
// replacement pipeline, and its paid sequential sibling is the live result reader.
fn departure_payment_move_runner(
    shape: u8,
    foreign: bool,
    empty: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, Comparator,
        DiscardSelfScope, Effect, FilterProp, QuantityExpr, ReplacementDefinition, ReplacementMode,
        SacrificeCost, SubAbilityLink, TargetFilter, TypedFilter, UnlessPayModifier,
    };
    departure_public_program_runner(foreign, empty, 2, |scenario| {
        let mut subjects = vec![];
        for suffix in if shape == 3 { vec![] } else { vec!["A", "B"] } {
            subjects.push(if shape == 2 {
                scenario.add_card_to_hand(
                    PlayerId(1),
                    &format!("Supplemental random payment {suffix}"),
                )
            } else {
                scenario
                    .add_artifact_from_oracle(
                        PlayerId(1),
                        &format!("Supplemental replacement payment {suffix}"),
                        "",
                    )
                    .id()
            });
        }
        let valid = TargetFilter::Or {
            filters: subjects
                .iter()
                .map(|id| TargetFilter::SpecificObject { id: *id })
                .collect(),
        };
        let replacement = if shape == 3 {
            ReplacementDefinition::new(engine::types::replacements::ReplacementEvent::AddCounter)
                .mode(ReplacementMode::MayCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::NoOp,
                    ))),
                })
        } else {
            ReplacementDefinition::new(engine::types::replacements::ReplacementEvent::Moved)
                .valid_card(valid)
                .destination_zone(if shape == 1 {
                    Zone::Hand
                } else {
                    Zone::Graveyard
                })
                .mode(ReplacementMode::MayCost {
                    cost: AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::NoOp,
                    ))),
                })
        };
        scenario
            .add_artifact_from_oracle(PlayerId(1), "Supplemental payment move replacement", "")
            .with_replacement_definition(replacement);
        let Effect::GainLife { amount, .. } = *departure_gain_definition(7, PlayerId(0)).effect
        else {
            unreachable!()
        };
        let mut typed = TypedFilter::default();
        typed.properties.push(FilterProp::Cmc {
            comparator: Comparator::LT,
            value: QuantityExpr::Sum {
                exprs: vec![amount, QuantityExpr::Fixed { value: 1 }],
            },
        });
        let filter = TargetFilter::Typed(typed);
        let cost = match shape {
            0 => AbilityCost::Sacrifice(SacrificeCost::count(filter, 2)),
            1 => AbilityCost::ReturnToHand {
                count: 2,
                filter: Some(filter),
                from_zone: None,
            },
            2 => AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                filter: Some(filter),
                selection: CardSelectionMode::Random,
                self_scope: DiscardSelfScope::FromHand,
            },
            3 => AbilityCost::EffectCost {
                effect: Box::new(Effect::PutCounter {
                    counter_type: CounterType::Generic("charge".into()),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }),
            },
            _ => unreachable!(),
        };
        let mut body = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 99 },
                player: TargetFilter::Controller,
            },
        );
        body.unless_pay = Some(UnlessPayModifier {
            cost,
            payer: TargetFilter::SpecificPlayer { id: PlayerId(1) },
        });
        let mut tail = departure_dual_reader();
        tail.sub_link = SubAbilityLink::SequentialSibling;
        body.sub_ability = Some(Box::new(tail));
        (body, subjects)
    })
}

fn departure_finish_payment_move(
    runner: &mut engine::game::scenario::GameRunner,
    subjects: &[engine::types::identifiers::ObjectId],
    mut reached: impl FnMut(&engine::game::scenario::GameRunner),
) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{PendingCostMoveResume, WaitingFor};
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment {
            player: PlayerId(1),
            ..
        }
    ));
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    let mut pauses = 0;
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::WardSacrificeChoice { permanents, .. }
            | WaitingFor::UnlessBounceChoice { permanents, .. } => {
                assert!(!permanents.is_empty());
                assert!(permanents.iter().all(|id| subjects.contains(id)));
                let id = *permanents.last().unwrap();
                runner
                    .act(GameAction::SelectCards { cards: vec![id] })
                    .unwrap();
            }
            WaitingFor::ReplacementChoice { player, .. } => {
                assert_eq!(*player, PlayerId(if subjects.is_empty() { 0 } else { 1 }));
                let (occurrence, body) =
                    match runner.state().pending_cost_move_resume.as_ref().unwrap() {
                        PendingCostMoveResume::WardSacrificePayment {
                            return_result_occurrence,
                            pending_effect,
                            ..
                        }
                        | PendingCostMoveResume::UnlessBouncePayment {
                            return_result_occurrence,
                            pending_effect,
                            ..
                        }
                        | PendingCostMoveResume::CounterAdditionUnlessPayment {
                            return_result_occurrence,
                            pending_effect,
                            ..
                        } => (*return_result_occurrence, pending_effect.as_ref()),
                        PendingCostMoveResume::RandomDiscardUnlessPayment(value) => (
                            value.return_result_occurrence,
                            value.pending_effect.as_ref(),
                        ),
                        other => {
                            panic!("the actual selected payment root remains parked: {other:?}")
                        }
                    };
                assert_eq!(
                    runner.state().return_result_frames[&occurrence.unwrap()].process,
                    body.return_result_process
                );
                assert_eq!(
                    runner.state().players[0].life,
                    20,
                    "no paid tail can run before its last required cost move settles"
                );
                reached(runner);
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect("each actual payment/replacement re-pause retains its parent O and bound body P");
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                pauses += 1;
            }
            WaitingFor::Priority { .. } => {
                assert_eq!(
                    pauses,
                    if subjects.is_empty() { 1 } else { 2 },
                    "both actual cost members separately traverse replacement suspension"
                );
                return;
            }
            other => panic!("unexpected payment stage {other:?}"),
        }
    }
    panic!("the two-member payment must finish");
}

#[test]
fn departure_payment_replacement_random_discard_ward_and_bounce_preserve_the_parent_until_its_paid_tail(
) {
    use engine::types::ability::ReturnResultId;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{
        GameState, InstructionResult, PendingCostMoveResume, RevealedInstructionResult,
    };
    for shape in 0..4 {
        let (mut foreign, foreign_spell, foreign_subjects) =
            departure_payment_move_runner(shape, true, false);
        let (&foreign_o, foreign_frame) =
            foreign.state().return_result_frames.iter().next().unwrap();
        let foreign_frame = foreign_frame.clone();
        departure_finish_payment_move(&mut foreign, &foreign_subjects, |_| {});
        departure_assert_program_complete(&foreign, foreign_spell, 7, 2);
        for empty in [false, true] {
            let (mut runner, spell, subjects) = departure_payment_move_runner(shape, false, empty);
            departure_finish_payment_move(&mut runner, &subjects, |reached| {
                let occurrence = match reached.state().pending_cost_move_resume.as_ref().unwrap() {
                    PendingCostMoveResume::WardSacrificePayment {
                        return_result_occurrence,
                        ..
                    }
                    | PendingCostMoveResume::UnlessBouncePayment {
                        return_result_occurrence,
                        ..
                    }
                    | PendingCostMoveResume::CounterAdditionUnlessPayment {
                        return_result_occurrence,
                        ..
                    } => return_result_occurrence.unwrap(),
                    PendingCostMoveResume::RandomDiscardUnlessPayment(value) => {
                        value.return_result_occurrence.unwrap()
                    }
                    _ => unreachable!(),
                };
                for fault in 0..6 {
                    let mut hostile = reached.state().clone();
                    match fault {
                        0 => {
                            hostile.return_result_frames.clear();
                            hostile
                                .return_result_frames
                                .insert(foreign_o, foreign_frame.clone());
                            hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                            hostile.next_return_result_process_id =
                                foreign_frame.process.unwrap().0 + 1;
                            let mut resume =
                                serde_json::to_value(&hostile.pending_cost_move_resume).unwrap();
                            departure_rewrite_lookup(
                                &mut resume,
                                &serde_json::json!(occurrence),
                                &serde_json::json!(foreign_o),
                            );
                            hostile.pending_cost_move_resume =
                                serde_json::from_value(resume).unwrap();
                            let mut stack =
                                serde_json::to_value(&hostile.resolution_stack).unwrap();
                            departure_rewrite_lookup(
                                &mut stack,
                                &serde_json::json!(occurrence),
                                &serde_json::json!(foreign_o),
                            );
                            hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                        }
                        1 | 2 => {
                            let (lookup, body) =
                                match hostile.pending_cost_move_resume.as_mut().unwrap() {
                                    PendingCostMoveResume::WardSacrificePayment {
                                        return_result_occurrence,
                                        pending_effect,
                                        ..
                                    }
                                    | PendingCostMoveResume::UnlessBouncePayment {
                                        return_result_occurrence,
                                        pending_effect,
                                        ..
                                    }
                                    | PendingCostMoveResume::CounterAdditionUnlessPayment {
                                        return_result_occurrence,
                                        pending_effect,
                                        ..
                                    } => (return_result_occurrence, pending_effect),
                                    PendingCostMoveResume::RandomDiscardUnlessPayment(value) => (
                                        &mut value.return_result_occurrence,
                                        &mut value.pending_effect,
                                    ),
                                    _ => unreachable!(),
                                };
                            if fault == 1 {
                                departure_clear_bound_process(body);
                            } else {
                                *lookup = None;
                            }
                        }
                        3 => {
                            hostile
                                .return_result_frames
                                .get_mut(&occurrence)
                                .unwrap()
                                .results
                                .remove(&ReturnResultId(7));
                        }
                        4 => {
                            hostile
                                .return_result_frames
                                .get_mut(&occurrence)
                                .unwrap()
                                .results
                                .insert(
                                    ReturnResultId(7),
                                    InstructionResult::RevealedCards(RevealedInstructionResult {
                                        revealed_cards: vec![],
                                    }),
                                );
                        }
                        5 => {
                            hostile.return_result_frames.remove(&occurrence);
                        }
                        _ => unreachable!(),
                    }
                    let before = serde_json::to_value(&hostile).unwrap();
                    assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                    let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
                    let error = bad
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap_err();
                    if fault < 2 {
                        assert!(format!("{error:?}").contains("process owner mismatch"));
                    }
                    assert_eq!(serde_json::to_value(bad.state()).unwrap(), before, "shape {shape}/fault {fault}: reject before replacement/member/RNG/counter/zone/event or paid-tail mutation");
                }
            });
            for id in subjects {
                assert_eq!(
                    runner.state().objects[&id].zone,
                    if shape == 1 {
                        Zone::Hand
                    } else {
                        Zone::Graveyard
                    }
                );
            }
            assert!(runner.state().pending_cost_move_resume.is_none());
            departure_assert_program_complete(
                &runner,
                spell,
                if empty { 0 } else { 5 },
                if empty { 0 } else { 2 },
            );
        }
    }
}

fn departure_payment_mana_child_runner(
    effect_cost: bool,
    foreign: bool,
    empty: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        QuantityExpr, ReplacementDefinition, ReplacementMode, SubAbilityLink, TargetFilter,
        UnlessPayModifier,
    };
    use engine::types::mana::{ManaColor, ManaCost};
    departure_public_program_runner(foreign, empty, 2, |scenario| {
        // This independent mana source never reads the parent result. Its ordinary
        // self-exile cost raises the real replacement suspension while the parent
        // retains an unpaid, dynamically evaluated life-cost suffix.
        let source = scenario
            .add_artifact_from_oracle(
                PlayerId(1),
                "Supplemental independent payment mana source",
                "",
            )
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
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
                        AbilityCost::Exile {
                            count: 1,
                            zone: None,
                            filter: Some(TargetFilter::SelfRef),
                        },
                    ],
                }),
            )
            .id();
        scenario
            .add_artifact_from_oracle(
                PlayerId(1),
                "Supplemental independent source cost replacement",
                "",
            )
            .with_replacement_definition(
                ReplacementDefinition::new(engine::types::replacements::ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SpecificObject { id: source })
                    .destination_zone(Zone::Exile)
                    .mode(ReplacementMode::MayCost {
                        cost: AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        decline: Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::NoOp,
                        ))),
                    }),
            );
        let Effect::GainLife { amount, .. } = *departure_gain_definition(7, PlayerId(0)).effect
        else {
            unreachable!()
        };
        let cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::PayLife { amount },
            ],
        };
        let mut body = if effect_cost {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PayCost {
                    cost,
                    scale: None,
                    payer: TargetFilter::SpecificPlayer { id: PlayerId(1) },
                },
            )
        } else {
            let mut guarded = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 99 },
                    player: TargetFilter::Controller,
                },
            );
            guarded.unless_pay = Some(UnlessPayModifier {
                cost,
                payer: TargetFilter::SpecificPlayer { id: PlayerId(1) },
            });
            guarded
        };
        let mut tail = departure_dual_reader();
        if !effect_cost {
            tail.sub_link = SubAbilityLink::SequentialSibling;
        }
        body.sub_ability = Some(Box::new(tail));
        (body, vec![source])
    })
}

fn departure_finish_payment_mana_child(
    runner: &mut engine::game::scenario::GameRunner,
    mut reached: impl FnMut(&engine::game::scenario::GameRunner),
) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ManaAbilityResume, PendingCostMoveResume, WaitingFor};
    let mut replacement_count = 0;
    for _ in 0..6 {
        match &runner.state().waiting_for {
            WaitingFor::UnlessPayment { player, .. } => {
                assert_eq!(*player, PlayerId(1));
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect(
                    "the actual unpaid parent root also survives its re-emitted payment prompt",
                );
                runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
            }
            WaitingFor::ReplacementChoice { player, .. } => {
                assert_eq!(*player, PlayerId(1));
                let Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) =
                    &runner.state().pending_cost_move_resume
                else {
                    panic!("a real mana source owns the actual replaceable cost cursor");
                };
                let (occurrence, parent) = match &pending.resume {
                    ManaAbilityResume::UnlessPayment { return_result_occurrence, pending_effect, .. } => (*return_result_occurrence, pending_effect),
                    ManaAbilityResume::EffectPayCost { return_result_occurrence, ability, .. } => (*return_result_occurrence, ability),
                    other => panic!("the exact resolution parent is captured, independently of the mana source: {other:?}"),
                };
                assert_eq!(
                    runner.state().return_result_frames[&occurrence.unwrap()].process,
                    parent.return_result_process
                );
                assert_eq!(runner.state().players[0].life, 20);
                assert_eq!(
                    runner.state().players[1].life,
                    20,
                    "the unpaid dynamic suffix cannot run during the child move"
                );
                assert_eq!(runner.state().players[1].mana_pool.total(), 0);
                reached(runner);
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect("nested PendingManaAbility resume and cost_move_resume retain exact parent custody");
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                replacement_count += 1;
            }
            WaitingFor::Priority { .. } => {
                assert_eq!(replacement_count, 1);
                return;
            }
            other => panic!("unexpected parent mana-payment stage {other:?}"),
        }
    }
    panic!("the single independent source and parent cost must finish once");
}

#[test]
fn departure_mana_payment_child_preserves_unpaid_dynamic_suffix_and_once_only_parent_rider() {
    use engine::types::ability::ReturnResultId;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{
        GameState, InstructionResult, ManaAbilityResume, PendingCostMoveResume,
        RevealedInstructionResult,
    };
    for effect_cost in [false, true] {
        let (mut foreign, foreign_spell, foreign_sources) =
            departure_payment_mana_child_runner(effect_cost, true, false);
        let (&foreign_o, foreign_frame) =
            foreign.state().return_result_frames.iter().next().unwrap();
        let foreign_frame = foreign_frame.clone();
        departure_finish_payment_mana_child(&mut foreign, |_| {});
        assert_eq!(
            foreign.state().objects[&foreign_sources[0]].zone,
            Zone::Exile
        );
        departure_assert_program_complete(&foreign, foreign_spell, 7, -5);
        for empty in [false, true] {
            let (mut runner, spell, sources) =
                departure_payment_mana_child_runner(effect_cost, false, empty);
            departure_finish_payment_mana_child(&mut runner, |reached| {
                let Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) =
                    &reached.state().pending_cost_move_resume
                else {
                    unreachable!()
                };
                let occurrence = match &pending.resume {
                    ManaAbilityResume::UnlessPayment {
                        return_result_occurrence,
                        ..
                    }
                    | ManaAbilityResume::EffectPayCost {
                        return_result_occurrence,
                        ..
                    } => return_result_occurrence.unwrap(),
                    _ => unreachable!(),
                };
                for fault in 0..5 {
                    let mut hostile = reached.state().clone();
                    if fault == 0 {
                        hostile.return_result_frames.clear();
                        hostile
                            .return_result_frames
                            .insert(foreign_o, foreign_frame.clone());
                        hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                        hostile.next_return_result_process_id =
                            foreign_frame.process.unwrap().0 + 1;
                        let mut payload =
                            serde_json::to_value(&hostile.pending_cost_move_resume).unwrap();
                        departure_rewrite_lookup(
                            &mut payload,
                            &serde_json::json!(occurrence),
                            &serde_json::json!(foreign_o),
                        );
                        hostile.pending_cost_move_resume = serde_json::from_value(payload).unwrap();
                        let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                        departure_rewrite_lookup(
                            &mut stack,
                            &serde_json::json!(occurrence),
                            &serde_json::json!(foreign_o),
                        );
                        hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                    } else if fault == 1 {
                        let Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) =
                            &mut hostile.pending_cost_move_resume
                        else {
                            unreachable!()
                        };
                        match &mut pending.resume {
                            ManaAbilityResume::UnlessPayment { pending_effect, .. }
                            | ManaAbilityResume::EffectPayCost {
                                ability: pending_effect,
                                ..
                            } => departure_clear_bound_process(pending_effect),
                            _ => unreachable!(),
                        }
                    } else if fault == 2 {
                        hostile.return_result_frames.remove(&occurrence);
                    } else if fault == 3 {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .results
                            .remove(&ReturnResultId(7));
                    } else {
                        hostile
                            .return_result_frames
                            .get_mut(&occurrence)
                            .unwrap()
                            .results
                            .insert(
                                ReturnResultId(7),
                                InstructionResult::RevealedCards(RevealedInstructionResult {
                                    revealed_cards: vec![],
                                }),
                            );
                    }
                    let before = serde_json::to_value(&hostile).unwrap();
                    assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                    let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
                    let error = bad
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap_err();
                    if fault < 2 {
                        assert!(format!("{error:?}").contains("process owner mismatch"));
                    }
                    assert_eq!(serde_json::to_value(bad.state()).unwrap(), before, "the independent source cannot spend, move, produce mana, pay the parent suffix or execute its rider using another process");
                }
            });
            assert_eq!(runner.state().objects[&sources[0]].zone, Zone::Exile);
            assert!(runner.state().objects[&sources[0]].tapped);
            assert_eq!(
                runner.state().players[1].mana_pool.total(),
                0,
                "the actual produced unit funds the same parent cost exactly once"
            );
            assert!(runner.state().pending_cost_move_resume.is_none());
            departure_assert_program_complete(
                &runner,
                spell,
                if empty { 0 } else { 5 },
                if empty { 0 } else { -3 },
            );
        }
    }
}

fn departure_optional_search_runner(
    tail: bool,
    foreign: bool,
    empty: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, Comparator, ControllerRef,
        Effect, FilterProp, PlayerFilter, PlayerRelation, QuantityExpr, QuantityRef,
        ReplacementDefinition, ReplacementMode, SearchSelectionConstraint, TargetFilter,
        TypedFilter,
    };
    use engine::types::events::PlayerActionKind;
    departure_public_program_runner(foreign, empty, 2, |scenario| {
        let mut subjects = vec![];
        for player in [PlayerId(0), PlayerId(1)] {
            for index in 0..10 {
                subjects.push(
                    scenario
                        .add_spell_to_library_top(
                            player,
                            &format!("Supplemental optional library {player:?}/{index}"),
                            false,
                        )
                        .id(),
                );
            }
        }
        if tail {
            scenario
                .add_artifact_from_oracle(
                    PlayerId(0),
                    "Supplemental scoped delivery replacement",
                    "",
                )
                .with_replacement_definition(
                    ReplacementDefinition::new(
                        engine::types::replacements::ReplacementEvent::Moved,
                    )
                    .valid_card(TargetFilter::Or {
                        filters: subjects
                            .iter()
                            .map(|id| TargetFilter::SpecificObject { id: *id })
                            .collect(),
                    })
                    .destination_zone(Zone::Hand)
                    .mode(ReplacementMode::MayCost {
                        cost: AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        decline: Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::NoOp,
                        ))),
                    }),
                );
        }
        let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, PlayerId(0)).effect
        else {
            unreachable!()
        };
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ZoneChangeAggregate {
                    filter: TargetFilter::Typed(former),
                    ..
                },
        } = &mut amount
        else {
            unreachable!()
        };
        former.controller = Some(ControllerRef::ScopedPlayer);
        let mut filter = TypedFilter::default();
        filter.properties.push(FilterProp::Cmc {
            comparator: Comparator::LT,
            value: QuantityExpr::Sum {
                exprs: vec![amount.clone(), QuantityExpr::Fixed { value: 1 }],
            },
        });
        let mut search = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                source_zones: vec![Zone::Library],
                filter: TargetFilter::Typed(filter),
                count: if tail {
                    QuantityExpr::Fixed { value: 1 }
                } else {
                    amount.clone()
                },
                reveal: true,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
            },
        );
        search.optional = true;
        search.player_scope = Some(PlayerFilter::All);
        let delivery: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZone","origin":"Library","destination":"Hand","target":{"type":"ParentTarget"}})).unwrap();
        let mut delivery = AbilityDefinition::new(AbilityKind::Spell, delivery);
        if tail {
            let mut shuffle = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Shuffle {
                    target: TargetFilter::Controller,
                },
            );
            shuffle.player_scope = Some(PlayerFilter::PerformedActionThisWay {
                relation: PlayerRelation::All,
                action: PlayerActionKind::SearchedLibrary,
            });
            shuffle.condition = Some(AbilityCondition::QuantityCheck {
                lhs: amount,
                comparator: Comparator::GT,
                rhs: QuantityExpr::Fixed { value: 0 },
            });
            delivery.sub_ability = Some(Box::new(shuffle));
        }
        search.sub_ability = Some(Box::new(delivery));
        (search, subjects)
    })
}

#[test]
fn departure_optional_scoped_search_prepares_unequal_live_counts_after_all_acceptances() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{GameState, WaitingFor};
    let (foreign, foreign_spell, _) = departure_optional_search_runner(false, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    let mut foreign = Some(foreign);
    for runner_foreign in [true, false] {
        // The genuine foreign producer is also completed through every public
        // acceptance and selection; it is not merely a plausible saved frame.
        let (mut runner, spell, _) = if runner_foreign {
            (foreign.take().unwrap(), foreign_spell, vec![])
        } else {
            departure_optional_search_runner(false, false, false)
        };
        let mut selected = 0;
        for player in [PlayerId(0), PlayerId(1)] {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: actual, .. } if actual == player)
            );
            assert!(
                runner.state().active_library_searches.is_empty(),
                "no early search preparation while a later acceptance is unknown"
            );
            if !runner_foreign {
                let mut hostile = runner.state().clone();
                let original = hostile
                    .pending_scoped_library_search
                    .as_ref()
                    .unwrap()
                    .return_result_occurrence
                    .unwrap();
                hostile.return_result_frames.clear();
                hostile
                    .return_result_frames
                    .insert(foreign_o, foreign_frame.clone());
                hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                hostile
                    .pending_scoped_library_search
                    .as_mut()
                    .unwrap()
                    .return_result_occurrence = Some(foreign_o);
                let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                departure_rewrite_lookup(
                    &mut stack,
                    &serde_json::json!(original),
                    &serde_json::json!(foreign_o),
                );
                hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                let before = serde_json::to_value(&hostile).unwrap();
                assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
                assert!(format!(
                    "{:?}",
                    bad.act(GameAction::DecideOptionalEffect { accept: true })
                        .unwrap_err()
                )
                .contains("process owner mismatch"));
                assert_eq!(serde_json::to_value(bad.state()).unwrap(), before, "reject before recording acceptance, preparing hidden candidates, revealing or moving cards");
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("late acceptance preserves typed count/filter and exact selected owner");
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
        }
        for player in [PlayerId(0), PlayerId(1)] {
            let WaitingFor::SearchChoice {
                player: actual,
                count,
                cards,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("each accepted nonempty scoped search reaches its real private choice");
            };
            assert_eq!(*actual, player);
            assert_eq!(
                *count,
                if player == PlayerId(0) {
                    if runner_foreign {
                        7
                    } else {
                        5
                    }
                } else {
                    2
                }
            );
            let chosen = cards[cards.len() - *count..].to_vec();
            assert!(chosen.len() > 1);
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect(
                "prepared choices and the custom scoped-search wire retain the same process",
            );
            runner
                .act(GameAction::SelectCards { cards: chosen })
                .unwrap();
            selected += 1;
        }
        assert_eq!(selected, 2);
        assert_eq!(
            runner.state().players[0].hand.len(),
            if runner_foreign { 7 } else { 5 }
        );
        assert_eq!(runner.state().players[1].hand.len(), 2);
        assert!(runner.state().pending_scoped_library_search.is_none());
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
    for decline_second in [false, true] {
        let (mut runner, spell, _) = departure_optional_search_runner(false, false, true);
        for player in [PlayerId(0), PlayerId(1)] {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: actual, .. } if actual == player)
            );
            runner
                .act(GameAction::DecideOptionalEffect {
                    accept: !(decline_second && player == PlayerId(1)),
                })
                .unwrap();
        }
        // A completed empty producer supplies known zero; no unavailable scalar
        // and no artificial nonzero search prompt substitutes for that result.
        if let WaitingFor::SearchChoice { count, .. } = runner.state().waiting_for {
            assert_eq!(count, 0);
        }
        while matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }) {
            runner
                .act(GameAction::SelectCards { cards: vec![] })
                .unwrap();
        }
        assert!(runner
            .state()
            .players
            .iter()
            .all(|player| player.hand.is_empty()));
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
}

fn departure_finish_optional_search_tail(
    runner: &mut engine::game::scenario::GameRunner,
    subjects: &[engine::types::identifiers::ObjectId],
    mut replacement: impl FnMut(&engine::game::scenario::GameRunner),
) -> usize {
    use engine::types::ability::EffectKind;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{BatchCompletion, LibrarySearchDeliveryResume, WaitingFor};
    let mut shuffles = 0;
    let mut deliveries = 0;
    for player in [PlayerId(0), PlayerId(1)] {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: actual, .. } if actual == player)
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
    }
    for player in [PlayerId(0), PlayerId(1)] {
        let WaitingFor::SearchChoice {
            player: actual,
            cards,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the optional accepted search enters its real scoped candidate choice");
        };
        assert_eq!(*actual, player);
        assert_eq!(*count, 1);
        let id = *cards.iter().rev().find(|id| subjects.contains(id)).unwrap();
        let result = runner
            .act(GameAction::SelectCards { cards: vec![id] })
            .unwrap();
        shuffles += result
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Shuffle,
                        ..
                    }
                )
            })
            .count();
    }
    for _ in 0..4 {
        match runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                let batch = runner.state().active_batch_delivery().expect(
                    "simultaneous library delivery owns the actual replacement-paused batch",
                );
                let Some(BatchCompletion::LibrarySearchDeliverySettled {
                    resume:
                        LibrarySearchDeliveryResume::Scoped {
                            return_result_occurrence: Some(occurrence),
                            after_scope: Some(tail),
                            ..
                        },
                }) = &batch.completion
                else {
                    panic!("the original narrow shuffle tail retains its exact process in the existing completion");
                };
                assert_eq!(
                    runner.state().return_result_frames[occurrence].process,
                    tail.return_result_process
                );
                assert_eq!(
                    shuffles, 0,
                    "the once-after-all tail cannot run at a delivery prefix"
                );
                replacement(runner);
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect(
                    "actual replacement-paused narrow delivery and its typed tail survive reload",
                );
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                shuffles += result
                    .events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            GameEvent::EffectResolved {
                                kind: EffectKind::Shuffle,
                                ..
                            }
                        )
                    })
                    .count();
                deliveries += 1;
            }
            WaitingFor::Priority { .. } => {
                assert_eq!(deliveries, 2);
                return shuffles;
            }
            ref other => panic!("unexpected scoped tail delivery stage {other:?}"),
        }
    }
    panic!("the finite simultaneous delivery must settle");
}

#[test]
fn departure_scoped_search_narrow_conditional_shuffle_tail_keeps_its_owner_through_actual_delivery_replacements(
) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::GameState;
    let (mut foreign, foreign_spell, subjects) =
        departure_optional_search_runner(true, true, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    assert_eq!(
        departure_finish_optional_search_tail(&mut foreign, &subjects, |_| {}),
        2
    );
    departure_assert_program_complete(&foreign, foreign_spell, 0, 0);
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_optional_search_runner(true, false, empty);
        let shuffles = departure_finish_optional_search_tail(&mut runner, &subjects, |reached| {
            let original = reached
                .state()
                .pending_scoped_library_search
                .as_ref()
                .unwrap()
                .return_result_occurrence
                .unwrap();
            for fault in 0..3 {
                let mut hostile = reached.state().clone();
                if fault == 0 {
                    hostile.return_result_frames.clear();
                    hostile
                        .return_result_frames
                        .insert(foreign_o, foreign_frame.clone());
                    hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                    hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                    hostile
                        .pending_scoped_library_search
                        .as_mut()
                        .unwrap()
                        .return_result_occurrence = Some(foreign_o);
                    let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                    departure_rewrite_lookup(
                        &mut stack,
                        &serde_json::json!(original),
                        &serde_json::json!(foreign_o),
                    );
                    hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                } else if fault == 1 {
                    let engine::types::game_state::BatchCompletion::LibrarySearchDeliverySettled {
                        resume:
                            engine::types::game_state::LibrarySearchDeliveryResume::Scoped {
                                after_scope: Some(tail),
                                ..
                            },
                    } = hostile
                        .active_batch_delivery_mut()
                        .unwrap()
                        .completion
                        .as_mut()
                        .unwrap()
                    else {
                        unreachable!()
                    };
                    departure_clear_bound_process(tail);
                } else {
                    hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .clear();
                }
                let before = serde_json::to_value(&hostile).unwrap();
                assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
                let error = bad
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap_err();
                if fault < 2 {
                    assert!(format!("{error:?}").contains("process owner mismatch"));
                }
                assert_eq!(
                    serde_json::to_value(bad.state()).unwrap(),
                    before,
                    "reject before delivery, reveal, choice or the narrow condition/shuffle reads"
                );
            }
        });
        assert_eq!(shuffles, if empty { 0 } else { 2 }, "the live saved conditional tail runs once per actual searcher, only after the complete delivery");
        assert_eq!(runner.state().players[0].hand.len(), 1);
        assert_eq!(runner.state().players[1].hand.len(), 1);
        assert!(runner.state().pending_library_search_delivery.is_none());
        assert!(runner.state().pending_scoped_library_search.is_none());
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
}

fn departure_selected_local_body(
    paused: bool,
    mixed: bool,
) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter, PtValue,
        QuantityExpr, QuantityRef, TargetFilter,
    };
    let mut read = departure_gain_definition(11, PlayerId(0));
    let Effect::GainLife { amount, .. } = read.effect.as_mut() else {
        unreachable!()
    };
    let QuantityExpr::Ref {
        qty:
            QuantityRef::ZoneChangeAggregate {
                filter: TargetFilter::Typed(former),
                ..
            },
    } = amount
    else {
        unreachable!()
    };
    former.controller = None;
    if mixed {
        let Effect::GainLife {
            amount: imported, ..
        } = *departure_gain_definition(7, PlayerId(0)).effect
        else {
            unreachable!()
        };
        *amount = QuantityExpr::Sum {
            exprs: vec![amount.clone(), imported],
        };
    }
    let reader = if paused {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![
                    AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                    read,
                ],
                kind: ChooseOneOfKind::Ordinary,
            },
        )
    } else {
        read
    };
    let mut producer = departure_exile_definition(11);
    producer.sub_ability = Some(Box::new(reader));
    let mut token = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Token {
            name: "Supplemental local three".into(),
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            types: vec!["Creature".into(), "Spirit".into()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        },
    );
    token.sub_ability = Some(Box::new(producer));
    token
}

fn departure_selected_family_definition(
    scenario: &mut engine::game::scenario::GameScenario,
    family: u8,
    paused: bool,
    mixed: bool,
) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, ControllerRef, DieResultBranch, Effect,
        PlayerFilter, PlayerScope, QuantityExpr, QuantityRef, TargetFilter, TieResolution,
        TypeFilter, TypedFilter, VoteSubject, VoteTally, VoteVisibility, VoterScope,
    };
    let local = departure_selected_local_body(paused, mixed);
    let effect = match family {
        0 => Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![
                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                local,
            ],
            kind: ChooseOneOfKind::Ordinary,
        },
        1 => {
            if paused {
                Effect::FlipCoins {
                    flips: engine::types::ability::CoinFlipRun::single(),
                    win_effect: Some(Box::new(local.clone())),
                    lose_effect: Some(Box::new(local)),
                    flipper: TargetFilter::Controller,
                }
            } else {
                Effect::FlipCoins {
                    flips: engine::types::ability::CoinFlipRun::Exactly {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    win_effect: Some(Box::new(local.clone())),
                    lose_effect: Some(Box::new(local)),
                    flipper: TargetFilter::Controller,
                }
            }
        }
        2 => Effect::RollDie {
            count: QuantityExpr::Fixed { value: 2 },
            sides: 1,
            results: vec![DieResultBranch {
                min: 1,
                max: 1,
                effect: Box::new(local),
            }],
            modifier: None,
        },
        3..=9 => {
            let mut branch = local;
            if family == 3 {
                branch.player_scope = Some(PlayerFilter::All);
            }
            if family == 4 {
                let Effect::Token { count, .. } = branch.effect.as_mut() else {
                    unreachable!()
                };
                *count = QuantityExpr::Ref {
                    qty: QuantityRef::VoteCount { choice_index: 1 },
                };
            }
            let subject = if family >= 8 {
                let candidates = (0..2)
                    .map(|index| {
                        scenario
                            .add_artifact_from_oracle(
                                PlayerId(0),
                                &format!("Supplemental vote object {index}"),
                                "",
                            )
                            .id()
                    })
                    .collect::<Vec<_>>();
                VoteSubject::Objects {
                    candidate_filter: TargetFilter::Or {
                        filters: candidates
                            .iter()
                            .map(|id| TargetFilter::SpecificObject { id: *id })
                            .collect(),
                    },
                    outcome_template: Box::new(branch.clone()),
                }
            } else {
                VoteSubject::Named
            };
            Effect::Vote {
                choices: if family >= 8 {
                    vec![]
                } else {
                    vec!["ordinary".into(), "addressed".into()]
                },
                per_choice_effect: if family >= 8 {
                    vec![]
                } else {
                    vec![Box::new(branch.clone()), Box::new(branch)]
                },
                starting_with: ControllerRef::You,
                voter_scope: VoterScope::AllPlayers,
                tally_mode: if family <= 5 {
                    VoteTally::PerVote
                } else {
                    VoteTally::TopVotes {
                        tie: if matches!(family, 6 | 8) {
                            TieResolution::Breaker(1)
                        } else {
                            TieResolution::AllTied
                        },
                    }
                },
                subject,
                visibility: VoteVisibility::Open,
            }
        }
        10 => {
            scenario.add_artifact_from_oracle(PlayerId(1), "Supplemental selected pile A", "");
            scenario.add_artifact_from_oracle(PlayerId(1), "Supplemental selected pile B", "");
            Effect::SeparateIntoPiles {
                partition_subject: VoterScope::EachOpponent,
                object_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                chooser: PlayerScope::Controller,
                chosen_pile_effect: Box::new(local.clone()),
                unchosen_pile_effect: Some(Box::new(local)),
                pile_source: engine::types::ability::PileSource::Battlefield,
            }
        }
        _ => unreachable!(),
    };
    let mut wrapper = AbilityDefinition::new(AbilityKind::Spell, effect);
    wrapper.sub_ability = Some(Box::new(departure_dual_reader()));
    wrapper
}

#[test]
fn departure_all_selected_family_entries_execute_complete_local_bodies_and_restore_the_outer_import(
) {
    use engine::types::ability::ReturnResultId;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, PileSide, WaitingFor};
    // This table reaches all thirteen selected entry calls: choice, coin, die,
    // player-scope/aggregate/initial-and-resumed ballot, four named/object top
    // tally arms, and both chosen/unchosen pile member bodies. The pausing rows
    // reach an actual local result owner before the imported outer tail runs.
    for family in 0..11 {
        for paused in [false, true] {
            if paused && matches!(family, 3 | 4 | 6 | 7 | 8 | 9 | 10) {
                continue;
            }
            for empty in [false, true] {
                let (mut runner, spell, _) =
                    departure_public_program_runner(false, empty, 2, |scenario| {
                        (
                            departure_selected_family_definition(scenario, family, paused, false),
                            vec![],
                        )
                    });
                let first_next_p = runner.state().next_return_result_process_id;
                let mut local_owners = std::collections::HashSet::new();
                let mut ballot = 0;
                let mut local_reads = 0;
                for _ in 0..24 {
                    let action = match &runner.state().waiting_for {
                        WaitingFor::Priority { .. } => break,
                        WaitingFor::ChooseOneOfBranch { player, branches, return_result_occurrence: Some(occurrence), expected_return_result_process: Some(process), .. } => {
                            assert_eq!(*player, PlayerId(0));
                            assert_eq!(branches.len(), 2);
                            let frame = &runner.state().return_result_frames[occurrence];
                            if let Some(InstructionResult::ZoneChanges(records)) = frame.results.get(&ReturnResultId(11)) {
                                assert_eq!(records.len(), 1); assert_eq!(records[0].from_zone, Some(Zone::Battlefield)); assert_eq!(records[0].to_zone, Zone::Exile);
                                assert_eq!(frame.process, Some(*process));
                                assert!(local_owners.insert((*occurrence, *process)), "each complete local invocation has a fresh independent owner");
                                assert_eq!(runner.state().return_result_frames.len(), 2, "the outer five remains separately owned until the local child's real pause drains");
                                local_reads += 1;
                            } else { assert_eq!(family, 0); assert!(!frame.results.contains_key(&ReturnResultId(11))); }
                            GameAction::ChooseBranch { index: 1 }
                        }
                        WaitingFor::VoteChoice { player, options, candidate_objects, .. } => {
                            assert_eq!(*player, PlayerId(ballot as u8));
                            assert_eq!(options.len(), 2);
                            let choice = if family >= 8 {
                                assert_eq!(candidate_objects.len(), 2);
                                ballot % 2
                            } else if matches!(family, 7) { ballot % 2 } else { 1 };
                            ballot += 1;
                            if family >= 8 { GameAction::SubmitVoteCandidate { candidate_index: choice as u32 } } else { GameAction::ChooseOption { choice: options[choice].clone() } }
                        }
                        WaitingFor::SeparatePilesPartition { player, eligible, .. } => { assert_eq!(*player, PlayerId(1)); assert_eq!(eligible.len(), 2); GameAction::SubmitPilePartition { pile_a: vec![eligible[1]] } }
                        WaitingFor::SeparatePilesChoice { player, current, .. } => { assert_eq!(*player, PlayerId(0)); assert_eq!(current.pile_a.len(), 1); assert_eq!(current.pile_b.len(), 1); GameAction::ChoosePile { pile: PileSide::B } },
                        other => panic!("selected family {family} reaches an unexpected public stage: {other:?}"),
                    };
                    let saved = serde_json::to_value(runner.state()).unwrap();
                    *runner.state_mut() = serde_json::from_value(saved).expect("future local definitions and actually bound local children reload without borrowing outer P");
                    runner.act(action).unwrap();
                }
                let invocations = if matches!(family, 0 | 6 | 8) || family == 1 && paused {
                    1
                } else {
                    2
                };
                if paused {
                    assert_eq!(local_reads, invocations);
                }
                // The aggregate-vote arm makes two three-power tokens in ONE
                // local process. All other double rows make one per invocation.
                let local_total = if family == 4 { 6 } else { invocations * 3 };
                assert!(runner.state().next_return_result_process_id >= first_next_p);
                assert!(runner
                    .state()
                    .objects
                    .values()
                    .all(|object| object.name != "Supplemental local three"
                        || object.zone != Zone::Battlefield));
                departure_assert_program_complete(
                    &runner,
                    spell,
                    local_total + if empty { 0 } else { 5 },
                    if empty { 0 } else { 2 },
                );
            }
        }
    }
}

#[test]
fn departure_each_selected_family_refuses_the_whole_mixed_root_before_either_producer() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{Effect, ResolvedAbility};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    for family in 0..11 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario
            .add_creature(P0, "Supplemental mixed outer five", 5, 5)
            .id();
        let selected = departure_selected_family_definition(&mut scenario, family, false, true);
        let mut root = departure_exile_definition(7);
        root.sub_ability = Some(Box::new(selected));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental refused whole selected program", false)
            .with_ability_definition(root.clone())
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        let canonical: ResolvedAbility = build_resolved_from_def(&root, spell, P0);
        let mut probe = runner.state().clone();
        let before = serde_json::to_value(&probe).unwrap();
        let mut events = vec![];
        let error = resolve_ability_chain(&mut probe, &canonical, &mut events, 0).unwrap_err();
        assert!(
            format!("{error:?}").contains("mixed local/imported instruction-result scope"),
            "family {family}: exact immutable ownership refusal"
        );
        assert_eq!(serde_json::to_value(&probe).unwrap(), before);
        assert!(events.is_empty());
        runner.cast(spell).commit();
        for _ in 0..8 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                && !runner.state().stack.is_empty()
            {
                runner.act(GameAction::PassPriority).unwrap();
            } else {
                break;
            }
        }
        assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield, "root A must not execute before discovering the selected body's foreign A + local B mix");
        assert!(runner
            .state()
            .objects
            .values()
            .all(|object| object.name != "Supplemental local three"));
        assert!(
            runner
                .state()
                .zone_changes_this_turn
                .iter()
                .all(|record| record.from_zone != Some(Zone::Battlefield)),
            "neither real A nor token/B departure can appear"
        );
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(runner.state().players[1].life, 20);
        assert_eq!(runner.state().next_return_result_occurrence_id, 1);
        assert_eq!(runner.state().next_return_result_process_id, 1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(matches!(*root.effect, Effect::ChangeZoneAll { .. }));
    }
}

fn departure_selected_import_family_definition(
    scenario: &mut engine::game::scenario::GameScenario,
    family: u8,
) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PlayerFilter, QuantityExpr, QuantityRef,
        TargetFilter, VoteSubject,
    };
    let mut wrapper = departure_selected_family_definition(scenario, family, false, false);
    let mut imported = departure_dual_reader();
    match wrapper.effect.as_mut() {
        Effect::ChooseOneOf { branches, .. } => branches[1] = departure_branch_reader(),
        Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            *win_effect = Some(Box::new(imported.clone()));
            *lose_effect = Some(Box::new(imported));
        }
        Effect::RollDie { results, .. } => *results[0].effect = departure_branch_reader(),
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            if family == 3 {
                imported.player_scope = Some(PlayerFilter::All);
            }
            if family == 4 {
                let mut aggregate = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::VoteCount { choice_index: 1 },
                        },
                        player: TargetFilter::SpecificPlayer { id: PlayerId(0) },
                    },
                );
                aggregate.sub_ability = Some(Box::new(imported));
                imported = aggregate;
            }
            if let VoteSubject::Objects {
                outcome_template, ..
            } = subject
            {
                **outcome_template = imported;
            } else {
                *per_choice_effect = vec![Box::new(imported.clone()), Box::new(imported)];
            }
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            **chosen_pile_effect = imported.clone();
            *unchosen_pile_effect = Some(Box::new(imported));
        }
        _ => unreachable!(),
    }
    wrapper
}

#[test]
fn departure_all_thirteen_selected_entries_keep_imports_separate_from_complete_local_processes() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{PileSide, WaitingFor};
    for family in 0..11 {
        for empty in [false, true] {
            let (mut runner, spell, _) =
                departure_public_program_runner(false, empty, 2, |scenario| {
                    (
                        departure_selected_import_family_definition(scenario, family),
                        vec![],
                    )
                });
            let mut ballots = 0;
            let mut child_reads = 0;
            for _ in 0..24 {
                let action = match &runner.state().waiting_for {
                    WaitingFor::Priority { .. } => break,
                    WaitingFor::ChooseOneOfBranch {
                        player, branches, ..
                    } => {
                        assert_eq!(*player, PlayerId(0));
                        assert_eq!(branches.len(), 2);
                        child_reads += 1;
                        GameAction::ChooseBranch { index: 1 }
                    }
                    WaitingFor::VoteChoice {
                        player,
                        options,
                        candidate_objects,
                        ..
                    } => {
                        assert_eq!(*player, PlayerId(ballots as u8));
                        assert_eq!(options.len(), 2);
                        let choice = if matches!(family, 7..=9) {
                            ballots % 2
                        } else {
                            1
                        };
                        ballots += 1;
                        if family >= 8 {
                            assert_eq!(candidate_objects.len(), 2);
                            GameAction::SubmitVoteCandidate {
                                candidate_index: choice as u32,
                            }
                        } else {
                            GameAction::ChooseOption {
                                choice: options[choice].clone(),
                            }
                        }
                    }
                    WaitingFor::SeparatePilesPartition {
                        player, eligible, ..
                    } => {
                        assert_eq!(*player, PlayerId(1));
                        assert_eq!(eligible.len(), 2);
                        GameAction::SubmitPilePartition {
                            pile_a: vec![eligible[1]],
                        }
                    }
                    WaitingFor::SeparatePilesChoice { player, .. } => {
                        assert_eq!(*player, PlayerId(0));
                        GameAction::ChoosePile { pile: PileSide::B }
                    }
                    other => {
                        panic!("actual import entry {family} reaches unexpected prompt {other:?}")
                    }
                };
                assert_eq!(runner.state().return_result_frames.len(), 1, "a read-only selected region reuses the exact completed producer, without allocating a local frame");
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect("every selected import prompt retains its actual expected process across reload");
                for view in [
                    engine::game::visibility::filter_state_for_viewer(runner.state(), PlayerId(0)),
                    engine::game::visibility::filter_state_for_viewer(runner.state(), PlayerId(1)),
                    engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
                ] {
                    assert!(view.return_result_frames.is_empty());
                    assert!(view.resolution_stack.is_empty());
                    assert!(!serde_json::to_string(&view)
                        .unwrap()
                        .contains("return_result_process"));
                }
                runner.act(action).unwrap();
            }
            let multiplier = if matches!(family, 0 | 4 | 6 | 8) {
                2
            } else {
                3
            };
            if family == 0 {
                assert_eq!(child_reads, 2);
            }
            if family == 2 {
                assert_eq!(child_reads, 2);
            }
            let extra_vote_gain = if family == 4 { 2 } else { 0 };
            assert_eq!(
                runner.state().next_return_result_process_id,
                2,
                "all selected readers use the original independently minted P"
            );
            departure_assert_program_complete(
                &runner,
                spell,
                if empty {
                    extra_vote_gain
                } else {
                    5 * multiplier + extra_vote_gain
                },
                if empty { 0 } else { 2 * multiplier },
            );
        }
    }
}

#[test]
fn departure_until_stop_reader_retains_its_bound_view_through_the_real_interactive_child() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, RepeatContinuation, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |_| {
            let mut body = departure_branch_reader();
            body.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            body.repeat_until = Some(RepeatContinuation::UntilStopConditions {
                stop_on_put_to_hand: true,
                stop_on_duplicate_exiled_names: false,
            });
            (body, vec![])
        });
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(runner.state().active_repeat_until().is_some());
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the actual until-stop parent and its read-only child preserve the same P");
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        assert_eq!(
            runner.state().players[0].hand.len(),
            1,
            "the actual following draw supplies the terminal put-to-hand witness"
        );
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

#[test]
fn departure_member_kind_optional_and_unless_iterations_execute_their_actually_bound_clones() {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Comparator, Effect, FilterProp, QuantityExpr,
        QuantityRef, TargetFilter, TypedFilter, UnlessPayModifier,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for kind in [false, true] {
        for unless in [false, true] {
            for empty in [false, true] {
                let (mut runner, spell, subjects) =
                    departure_public_program_runner(false, empty, 2, |scenario| {
                        let first = scenario
                            .add_artifact_from_oracle(
                                PlayerId(0),
                                "Supplemental iterated first",
                                "",
                            )
                            .id();
                        let second = scenario
                            .add_artifact_from_oracle(
                                PlayerId(0),
                                "Supplemental iterated second",
                                "",
                            )
                            .id();
                        if kind {
                            scenario.with_counter(first, CounterType::Generic("charge".into()), 1);
                            scenario.with_counter(
                                second,
                                CounterType::Generic("storage".into()),
                                1,
                            );
                        }
                        let Effect::GainLife { amount, .. } =
                            *departure_gain_definition(7, PlayerId(0)).effect
                        else {
                            unreachable!()
                        };
                        let filter = TargetFilter::And {
                            filters: vec![
                                TargetFilter::Or {
                                    filters: vec![
                                        TargetFilter::SpecificObject { id: first },
                                        TargetFilter::SpecificObject { id: second },
                                    ],
                                },
                                TargetFilter::Typed({
                                    let mut f = TypedFilter::default();
                                    f.properties.push(FilterProp::Cmc {
                                        comparator: Comparator::LT,
                                        value: QuantityExpr::Sum {
                                            exprs: vec![
                                                amount.clone(),
                                                QuantityExpr::Fixed { value: 1 },
                                            ],
                                        },
                                    });
                                    f
                                }),
                            ],
                        };
                        let mut body = AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::PutCounter {
                                counter_type: CounterType::Plus1Plus1,
                                count: amount,
                                target: if kind {
                                    TargetFilter::SpecificObject { id: first }
                                } else {
                                    TargetFilter::ParentTarget
                                },
                            },
                        );
                        body.repeat_for = Some(QuantityExpr::Ref {
                            qty: if kind {
                                QuantityRef::DistinctCounterKindsAmong { filter }
                            } else {
                                QuantityRef::ObjectCount { filter }
                            },
                        });
                        if unless {
                            body.unless_pay = Some(UnlessPayModifier {
                                cost: AbilityCost::PayLife {
                                    amount: QuantityExpr::Fixed { value: 1 },
                                },
                                payer: TargetFilter::Controller,
                            });
                        } else {
                            body.optional = true;
                        }
                        (body, vec![first, second])
                    });
                let mut answers = 0;
                for _ in 0..5 {
                    let action = match runner.state().waiting_for {
                        WaitingFor::OptionalEffectChoice { player: PlayerId(0), .. } if !unless => GameAction::DecideOptionalEffect { accept: true },
                        WaitingFor::UnlessPayment { player: PlayerId(0), .. } if unless => GameAction::PayUnlessCost { pay: false },
                        WaitingFor::Priority { .. } => break,
                        ref other => panic!("the actual member/kind iteration reaches its own public gate: {other:?}"),
                    };
                    assert_eq!(runner.state().players[0].life, 20);
                    let saved = serde_json::to_value(runner.state()).unwrap();
                    *runner.state_mut() = serde_json::from_value(saved).expect("each optional/unless iteration saves its actually bound future counter reader");
                    runner.act(action).unwrap();
                    answers += 1;
                }
                assert_eq!(answers, 2, "both distinct members/kinds run their actual gate and resume, rather than sharing an old unbound body");
                let expected = if empty { 0 } else { 5 };
                assert_eq!(
                    runner.state().objects[&subjects[0]]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                    if kind { 2 * expected } else { expected }
                );
                assert_eq!(
                    runner.state().objects[&subjects[1]]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                    if kind { 0 } else { expected }
                );
                departure_assert_program_complete(&runner, spell, 0, 0);
            }
        }
    }
}

#[test]
fn departure_numeric_scope_repeat_runs_both_bound_seats_on_both_public_iterations() {
    use engine::types::ability::{PlayerFilter, QuantityExpr};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |_| {
            let mut body = departure_branch_reader();
            body.player_scope = Some(PlayerFilter::All);
            body.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
            (body, vec![])
        });
        let mut saved_authority = None;
        for seat in [0, 1, 0, 1] {
            let WaitingFor::ChooseOneOfBranch {
                player,
                return_result_occurrence: Some(occurrence),
                expected_return_result_process: Some(process),
                ..
            } = runner.state().waiting_for
            else {
                panic!("both scoped seats of both counted iterations reach the real selected-body entry");
            };
            assert_eq!(player, PlayerId(seat));
            if let Some(old) = saved_authority {
                assert_eq!(old, (occurrence, process));
            } else {
                saved_authority = Some((occurrence, process));
            }
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect("numeric outer repeat, resumed repeat frame and per-seat imports agree on the actual bound clone");
            runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if empty { 0 } else { 20 },
            if empty { 0 } else { 8 },
        );
    }
}

fn departure_token_host_runner(
    foreign: bool,
    empty: bool,
    ordinary: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter, PtValue,
        QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectIncarnationRef;
    let (mut runner, spell, hosts) =
        departure_public_program_runner(foreign, empty, 2, |scenario| {
            let hosts = vec![
                scenario
                    .add_creature(PlayerId(0), "Supplemental host first", 2, 2)
                    .id(),
                scenario
                    .add_creature(PlayerId(0), "Supplemental host second", 3, 3)
                    .id(),
            ];
            let Effect::GainLife { amount, .. } = *departure_gain_definition(7, PlayerId(0)).effect
            else {
                unreachable!()
            };
            let token = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Token {
                    name: "Supplemental canonical host Aura".into(),
                    power: PtValue::Fixed(0),
                    toughness: PtValue::Fixed(0),
                    types: vec!["Enchantment".into(), "Aura".into()],
                    colors: vec![],
                    keywords: vec![engine::types::keywords::Keyword::Enchant(
                        TargetFilter::Typed(TypedFilter::creature()),
                    )],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: Some(TargetFilter::ParentTarget),
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![(
                        CounterType::Plus1Plus1,
                        if ordinary {
                            QuantityExpr::Fixed { value: 1 }
                        } else {
                            amount
                        },
                    )],
                },
            );
            (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![
                            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                            token,
                        ],
                        kind: ChooseOneOfKind::Ordinary,
                    },
                ),
                hosts,
            )
        });
    let pins = hosts
        .iter()
        .map(|id| ObjectIncarnationRef {
            object_id: *id,
            incarnation: runner.state().objects[id].incarnation,
        })
        .collect();
    let WaitingFor::ChooseOneOfBranch { context, .. } = &mut runner.state_mut().waiting_for else {
        panic!("the real paid producer reaches the canonical branch which supplies its batch-context fixture");
    };
    // Supplemental typed context input to the existing branch converter: these
    // are actual current host pins, not a parked token prompt or injected result.
    // No printed boon or source grammar is claimed for this composition.
    context.boon_trigger_batch_objects = pins;
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved)
        .expect("the canonical branch input and actual current host pins reload");
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::ChooseTokenHost { player: PlayerId(0), legal_targets, .. } if legal_targets.len() == 2)
    );
    (runner, spell, hosts)
}

#[test]
fn departure_token_host_public_resume_reads_the_actual_bound_counter_after_nondefault_host_selection(
) {
    use engine::types::ability::{ReturnResultId, TargetRef};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{
        GameState, InstructionResult, RevealedInstructionResult, WaitingFor,
    };
    let (mut foreign, foreign_spell, foreign_hosts) =
        departure_token_host_runner(true, false, false);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    let result = foreign
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(foreign_hosts[1])),
        })
        .unwrap();
    let token = result
        .events
        .iter()
        .find_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .expect("the public foreign seven body really creates the actual token");
    assert_eq!(
        foreign.state().objects[&token].counters[&CounterType::Plus1Plus1],
        7
    );
    departure_assert_program_complete(&foreign, foreign_spell, 0, 0);
    for empty in [false, true] {
        let (mut runner, spell, hosts) = departure_token_host_runner(false, empty, false);
        departure_assert_private_result_views(runner.state());
        let WaitingFor::ChooseTokenHost {
            return_result_occurrence: Some(occurrence),
            pending_ability,
            legal_targets,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the final host owner independently retains O and its bound executable P");
        };
        let occurrence = *occurrence;
        assert_eq!(
            runner.state().return_result_frames[&occurrence].process,
            pending_ability.return_result_process
        );
        assert_eq!(
            legal_targets,
            &[TargetRef::Object(hosts[0]), TargetRef::Object(hosts[1])]
        );
        assert!(runner
            .state()
            .resolution_stack
            .iter()
            .find_map(|frame| match frame {
                engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) =>
                    Some(&frame.pending),
                _ => None,
            })
            .is_none());
        for fault in 0..6 {
            let mut hostile = runner.state().clone();
            match fault {
                0 => {
                    hostile.return_result_frames.clear();
                    hostile
                        .return_result_frames
                        .insert(foreign_o, foreign_frame.clone());
                    hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                    hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                    if let WaitingFor::ChooseTokenHost {
                        return_result_occurrence,
                        ..
                    } = &mut hostile.waiting_for
                    {
                        *return_result_occurrence = Some(foreign_o);
                    }
                }
                1 => {
                    if let WaitingFor::ChooseTokenHost {
                        pending_ability, ..
                    } = &mut hostile.waiting_for
                    {
                        departure_clear_bound_process(pending_ability);
                    }
                }
                2 => {
                    if let WaitingFor::ChooseTokenHost {
                        return_result_occurrence,
                        ..
                    } = &mut hostile.waiting_for
                    {
                        *return_result_occurrence = None;
                    }
                }
                3 => {
                    hostile.return_result_frames.remove(&occurrence);
                }
                4 => {
                    hostile
                        .return_result_frames
                        .get_mut(&occurrence)
                        .unwrap()
                        .results
                        .remove(&ReturnResultId(7));
                }
                5 => {
                    hostile
                        .return_result_frames
                        .get_mut(&occurrence)
                        .unwrap()
                        .results
                        .insert(
                            ReturnResultId(7),
                            InstructionResult::RevealedCards(RevealedInstructionResult {
                                revealed_cards: vec![],
                            }),
                        );
                }
                _ => unreachable!(),
            }
            let before = serde_json::to_value(&hostile).unwrap();
            assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
            let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
            let error = bad
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(hosts[1])),
                })
                .unwrap_err();
            if fault < 2 {
                assert!(format!("{error:?}").contains("process owner mismatch"));
            }
            assert_eq!(serde_json::to_value(bad.state()).unwrap(), before, "reject before consuming the host choice or proposing/allocating/countering/attaching a token");
        }
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("the sole actual host owner survives canonical reload");
        let result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(hosts[1])),
            })
            .unwrap();
        let token = result
            .events
            .iter()
            .find_map(|event| {
                if let GameEvent::TokenCreated { object_id, .. } = event {
                    Some(*object_id)
                } else {
                    None
                }
            })
            .expect("the selected nondefault host causes exactly one real token creation");
        let object = &runner.state().objects[&token];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(
            object
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            if empty { 0 } else { 5 }
        );
        assert_eq!(
            object.attached_to,
            Some(engine::game::game_object::AttachTarget::Object(hosts[1]))
        );
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
    let (mut ordinary, spell, hosts) = departure_token_host_runner(false, false, true);
    assert!(
        matches!(ordinary.state().waiting_for, WaitingFor::ChooseTokenHost { return_result_occurrence: None, ref pending_ability, .. } if pending_ability.return_result_process.is_none())
    );
    let mut wire = serde_json::to_value(ordinary.state()).unwrap();
    wire["waiting_for"]["data"]
        .as_object_mut()
        .unwrap()
        .remove("return_result_occurrence");
    wire["waiting_for"]["data"]["pending_ability"]
        .as_object_mut()
        .unwrap()
        .remove("return_result_process");
    *ordinary.state_mut() = serde_json::from_value(wire)
        .expect("ordinary no-read legacy host prompts need no imported capability");
    let result = ordinary
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(hosts[1])),
        })
        .unwrap();
    let token = result
        .events
        .iter()
        .find_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        ordinary.state().objects[&token].counters[&CounterType::Plus1Plus1],
        1
    );
    departure_assert_program_complete(&ordinary, spell, 0, 0);
}

#[test]
fn departure_synchronous_publication_keeps_prior_unaddressed_and_two_declared_instructions_distinct(
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter, QuantityExpr,
        ReturnResultId, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let unrelated = scenario
        .add_creature(P0, "Supplemental earlier unrelated nine", 9, 9)
        .id();
    let a = [
        scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
        scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
    ];
    let b = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut first = departure_exile_definition(7);
    let Effect::ChangeZoneAll { target, .. } = first.effect.as_mut() else {
        unreachable!()
    };
    *target = TargetFilter::Or {
        filters: a
            .iter()
            .map(|id| TargetFilter::SpecificObject { id: *id })
            .collect(),
    };
    let mut second = departure_exile_definition(11);
    let Effect::ChangeZoneAll { target, .. } = second.effect.as_mut() else {
        unreachable!()
    };
    *target = TargetFilter::SpecificObject { id: b };
    let mut sum = departure_gain_definition(7, P0);
    let Effect::GainLife { amount, .. } = sum.effect.as_mut() else {
        unreachable!()
    };
    let Effect::GainLife { amount: other, .. } = *departure_gain_definition(11, P1).effect else {
        unreachable!()
    };
    *amount = QuantityExpr::Sum {
        exprs: vec![amount.clone(), other],
    };
    second.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![
                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                sum,
            ],
            kind: ChooseOneOfKind::Ordinary,
        },
    )));
    first.sub_ability = Some(Box::new(second));
    let prior: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZone", "origin":"Battlefield", "destination":"Exile", "target":{"type":"SpecificObject", "id":unrelated}})).unwrap();
    let mut root = AbilityDefinition::new(AbilityKind::Spell, prior);
    root.sub_ability = Some(Box::new(first));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental synchronous exact publications", false)
        .with_ability_definition(root)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.act(GameAction::PassPriority).unwrap();
        } else {
            break;
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { player: P0, .. }
    ));
    let frame = runner.state().return_result_frames.values().next().unwrap();
    assert_eq!(frame.results.len(), 2);
    for (id, expected) in [
        (ReturnResultId(7), a.to_vec()),
        (ReturnResultId(11), vec![b]),
    ] {
        let InstructionResult::ZoneChanges(records) = &frame.results[&id] else {
            unreachable!()
        };
        assert_eq!(
            records
                .iter()
                .map(|record| record.object_id)
                .collect::<std::collections::HashSet<_>>(),
            expected.into_iter().collect()
        );
        assert!(records
            .iter()
            .all(|record| record.from_zone == Some(Zone::Battlefield)
                && record.to_zone == Zone::Exile
                && record.object_id != unrelated));
    }
    assert_eq!(runner.state().objects[&unrelated].zone, Zone::Exile);
    assert_eq!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|record| record.from_zone == Some(Zone::Battlefield)
                && record.to_zone == Zone::Exile)
            .count(),
        4
    );
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved)
        .expect("two completed exact declarations coexist in one owning process");
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    departure_assert_program_complete(&runner, spell, 8, 0);
}

#[test]
fn departure_mass_entry_publication_excludes_genuinely_prohibited_members_and_publishes_empty_only_at_completion(
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef,
        ReplacementDefinition, ReplacementMode, ReturnResultId, StaticDefinition, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    for block_all in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let members = [
            scenario
                .add_creature_to_graveyard(P0, "Centaur Courser", 3, 3)
                .id(),
            scenario
                .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
                .id(),
            scenario
                .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
                .id(),
        ];
        let blocked = if block_all {
            members.to_vec()
        } else {
            vec![members[0]]
        };
        scenario
            .add_artifact_from_oracle(P0, "Supplemental canonical entry prohibition", "")
            .with_static_definition(
                StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom).affected(
                    TargetFilter::Or {
                        filters: blocked
                            .iter()
                            .map(|id| TargetFilter::SpecificObject { id: *id })
                            .collect(),
                    },
                ),
            );
        if !block_all {
            scenario
                .add_artifact_from_oracle(P0, "Supplemental entry member replacement", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(
                        engine::types::replacements::ReplacementEvent::Moved,
                    )
                    .destination_zone(Zone::Battlefield)
                    .valid_card(TargetFilter::Or {
                        filters: members[1..]
                            .iter()
                            .map(|id| TargetFilter::SpecificObject { id: *id })
                            .collect(),
                    })
                    .mode(ReplacementMode::MayCost {
                        cost: AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        decline: Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::NoOp,
                        ))),
                    }),
                );
        }
        for player in [P0, P1] {
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Library witness");
            }
        }
        let effect: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZoneAll", "origin":"Graveyard", "destination":"Battlefield", "target":{"type":"Or", "filters":members.iter().map(|id| serde_json::json!({"type":"SpecificObject", "id":id})).collect::<Vec<_>>()}})).unwrap();
        let mut root = AbilityDefinition::new(AbilityKind::Spell, effect);
        root.declares_return_result = Some(ReturnResultId(7));
        let mut choice = departure_branch_reader();
        let Effect::ChooseOneOf { branches, .. } = choice.effect.as_mut() else {
            unreachable!()
        };
        let first = &mut branches[1];
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { from, to, .. },
                },
            ..
        } = first.effect.as_mut()
        else {
            unreachable!()
        };
        *from = Some(Zone::Graveyard);
        *to = Some(Zone::Battlefield);
        let second = first.sub_ability.as_mut().unwrap();
        let Effect::GainLife {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { from, to, .. },
                },
            ..
        } = second.effect.as_mut()
        else {
            unreachable!()
        };
        *from = Some(Zone::Graveyard);
        *to = Some(Zone::Battlefield);
        root.sub_ability = Some(Box::new(choice));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental prohibited mass entry result", false)
            .with_ability_definition(root)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        runner.cast(spell).commit();
        let mut pauses = 0;
        for _ in 0..12 {
            match runner.state().waiting_for {
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                WaitingFor::ReplacementChoice { .. } => {
                    assert!(runner.state().return_result_frames.values().all(|frame| !frame.results.contains_key(&ReturnResultId(7))), "neither the prohibited member nor an actual delivered prefix is a completed result");
                    let saved = serde_json::to_value(runner.state()).unwrap();
                    *runner.state_mut() = serde_json::from_value(saved).expect("an owned unpublished mass-entry producer and its following reader remain coherent through each real member pause");
                    runner
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap();
                    pauses += 1;
                }
                WaitingFor::ChooseOneOfBranch { .. } => break,
                ref other => panic!("unexpected mass entry stage {other:?}"),
            }
        }
        assert_eq!(pauses, if block_all { 0 } else { 2 });
        let frame = runner.state().return_result_frames.values().next().unwrap();
        let InstructionResult::ZoneChanges(records) = &frame.results[&ReturnResultId(7)] else {
            unreachable!()
        };
        assert_eq!(records.len(), if block_all { 0 } else { 2 });
        assert!(records
            .iter()
            .all(|record| !blocked.contains(&record.object_id)));
        assert_eq!(runner.state().objects[&members[0]].zone, Zone::Graveyard);
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        for id in &members[1..] {
            assert_eq!(
                runner.state().objects[id].zone,
                if block_all {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
        }
        departure_assert_program_complete(
            &runner,
            spell,
            if block_all { 0 } else { 2 },
            if block_all { 0 } else { 2 },
        );
    }
}

fn departure_clear_bound_process(body: &mut engine::types::ability::ResolvedAbility) {
    body.return_result_process = None;
    if let Some(child) = body.sub_ability.as_deref_mut() {
        departure_clear_bound_process(child);
    }
    if let Some(child) = body.else_ability.as_deref_mut() {
        departure_clear_bound_process(child);
    }
}

#[test]
fn departure_latched_counter_queues_ignore_unconsumed_quantity_payloads_and_keep_only_the_true_tail_owner(
) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CounterMoveSelection, CounterTransferMode, Effect,
        QuantityExpr, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{CounterMoveChoice, CounterRemoveChoice, WaitingFor};
    let charge = CounterType::Generic("charge".into());
    let storage = CounterType::Generic("storage".into());
    for moving in [false, true] {
        for tail in [false, true] {
            for empty in [false, true] {
                let (mut runner, spell, subjects) =
                    departure_public_program_runner(false, empty, 2, |scenario| {
                        let source = scenario
                            .add_artifact_from_oracle(
                                PlayerId(0),
                                "Supplemental latched counter source",
                                "",
                            )
                            .id();
                        let first = scenario
                            .add_artifact_from_oracle(
                                PlayerId(0),
                                "Supplemental latched counter destination A",
                                "",
                            )
                            .id();
                        let second = scenario
                            .add_artifact_from_oracle(
                                PlayerId(0),
                                "Supplemental latched counter destination B",
                                "",
                            )
                            .id();
                        scenario.with_counter(source, charge.clone(), 4);
                        scenario.with_counter(source, storage.clone(), 3);
                        let effect = if moving {
                            Effect::MoveCounters {
                                source: TargetFilter::SpecificObject { id: source },
                                counter_type: None,
                                count: None,
                                mode: CounterTransferMode::Move,
                                selection: CounterMoveSelection::ResolutionDistributionAnyNumber,
                                target: TargetFilter::Or {
                                    filters: vec![
                                        TargetFilter::SpecificObject { id: first },
                                        TargetFilter::SpecificObject { id: second },
                                    ],
                                },
                            }
                        } else {
                            Effect::RemoveCounter {
                                counter_type: None,
                                count: QuantityExpr::UpTo {
                                    max: Box::new(QuantityExpr::Fixed { value: -1 }),
                                },
                                target: TargetFilter::SpecificObject { id: source },
                            }
                        };
                        let mut body = AbilityDefinition::new(AbilityKind::Spell, effect);
                        if tail {
                            body.sub_ability = Some(Box::new(departure_dual_reader()));
                        }
                        (body, vec![source, first, second])
                    });
                let data = serde_json::to_value(&runner.state().waiting_for).unwrap();
                assert!(
                    data["data"].get("return_result_occurrence").is_none(),
                    "scalar queues acquire no fabricated zone-result selector"
                );
                let pending = match &mut runner.state_mut().waiting_for {
                    WaitingFor::MoveCountersDistribution {
                        player,
                        available,
                        destinations,
                        pending_effect,
                        ..
                    } if moving => {
                        assert_eq!(*player, PlayerId(0));
                        assert_eq!(available.len(), 2);
                        assert_eq!(destinations.len(), 2);
                        pending_effect
                    }
                    WaitingFor::RemoveCountersChoice {
                        player,
                        available,
                        pending_effect,
                        ..
                    } if !moving => {
                        assert_eq!(*player, PlayerId(0));
                        assert_eq!(available.len(), 2);
                        pending_effect
                    }
                    other => {
                        panic!("the actual nontrivial scalar chooser must be reached: {other:?}")
                    }
                };
                let Effect::GainLife {
                    amount: ignored_foreign,
                    ..
                } = *departure_gain_definition(99, PlayerId(0)).effect
                else {
                    unreachable!()
                };
                match &mut pending.effect {
                    Effect::MoveCounters { count, .. } => *count = Some(ignored_foreign),
                    Effect::RemoveCounter { count, .. } => *count = ignored_foreign,
                    _ => unreachable!(),
                }
                // Only this ignored presentation/source payload is malformed.
                // Its real downstream reader remains separately bound in the
                // existing continuation; the scalar queue must never re-run it.
                pending.sub_ability = None;
                assert_eq!(runner.state().return_result_frames.is_empty(), !tail);
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).expect("latched legal budgets, not ignored typed quantities, authorize the actual scalar response");
                let action = if moving {
                    GameAction::ChooseCounterMoveDistribution {
                        selections: vec![
                            CounterMoveChoice {
                                destination_id: subjects[2],
                                counter_type: charge.clone(),
                                count: 2,
                            },
                            CounterMoveChoice {
                                destination_id: subjects[1],
                                counter_type: storage.clone(),
                                count: 1,
                            },
                        ],
                    }
                } else {
                    GameAction::ChooseCountersToRemove {
                        selections: vec![
                            CounterRemoveChoice {
                                counter_type: charge.clone(),
                                count: 2,
                            },
                            CounterRemoveChoice {
                                counter_type: storage.clone(),
                                count: 1,
                            },
                        ],
                    }
                };
                runner.act(action).unwrap();
                assert_eq!(runner.state().objects[&subjects[0]].counters[&charge], 2);
                assert_eq!(runner.state().objects[&subjects[0]].counters[&storage], 2);
                if moving {
                    assert_eq!(runner.state().objects[&subjects[2]].counters[&charge], 2);
                    assert_eq!(runner.state().objects[&subjects[1]].counters[&storage], 1);
                }
                departure_assert_program_complete(
                    &runner,
                    spell,
                    if tail && !empty { 5 } else { 0 },
                    if tail && !empty { 2 } else { 0 },
                );
            }
        }
    }
}

fn departure_return_as_aura_runner(
    foreign_payload: u8,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
    engine::types::ability::AbilityDefinition,
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, ContinuousModification, Effect, FilterProp,
        TargetFilter, TypedFilter,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victims = [
        scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
        scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
        scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
    ];
    let hosts = vec![
        scenario
            .add_artifact_from_oracle(P0, "Supplemental ordinary Aura host A", "")
            .id(),
        scenario
            .add_artifact_from_oracle(P0, "Supplemental ordinary Aura host B", "")
            .id(),
    ];
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Library witness");
        }
    }
    let mut root = departure_exile_definition(7);
    root.kind = AbilityKind::Activated;
    root.activation_zone = Some(Zone::Graveyard);
    root.cost = Some(engine::types::ability::AbilityCost::Mana {
        cost: ManaCost::generic(1),
    });
    let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
        unreachable!()
    };
    *target = TargetFilter::Or {
        filters: victims
            .iter()
            .map(|id| TargetFilter::SpecificObject { id: *id })
            .collect(),
    };
    let mut granted = departure_selected_local_body(false, false);
    granted.kind = AbilityKind::Activated;
    if foreign_payload == 2 {
        granted = departure_gain_definition(7, P0);
        granted.kind = AbilityKind::Activated;
    }
    let mut enchant = TargetFilter::Or {
        filters: hosts
            .iter()
            .map(|id| TargetFilter::SpecificObject { id: *id })
            .collect(),
    };
    if foreign_payload == 1 {
        let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect else {
            unreachable!()
        };
        let mut filter = TypedFilter::default();
        filter.properties.push(FilterProp::Cmc {
            comparator: Comparator::LT,
            value: amount,
        });
        enchant = TargetFilter::Typed(filter);
    }
    let mut aura = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ReturnAsAura {
            enchant_filter: enchant,
            grants: vec![
                ContinuousModification::RemoveAllAbilities,
                ContinuousModification::GrantAbility {
                    definition: Box::new(granted),
                },
            ],
        },
    );
    aura.sub_ability = Some(Box::new(departure_dual_reader()));
    let effect: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZone", "origin":"Graveyard", "destination":"Battlefield", "target":{"type":"SelfRef"}})).unwrap();
    let mut returned = AbilityDefinition::new(AbilityKind::Spell, effect);
    returned.sub_ability = Some(Box::new(aura));
    root.sub_ability = Some(Box::new(returned));
    let source = scenario
        .add_creature_to_graveyard(P0, "Supplemental canonical returned grant source", 1, 1)
        .with_ability_definition(root.clone())
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    (scenario.build(), source, hosts, root)
}

#[test]
fn departure_return_aura_latched_host_and_independent_granted_local_process_do_not_capture_the_callers_result(
) {
    use engine::types::ability::{Effect, TargetRef};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    let (mut runner, source, hosts, root) = departure_return_as_aura_runner(0);
    engine::types::ability::validate_instruction_result_definition(&root).expect(
        "an ordinary persistent enchant filter and complete independent local grant are admitted",
    );
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.act(GameAction::PassPriority).unwrap();
        } else {
            break;
        }
    }
    let WaitingFor::ReturnAsAuraTarget {
        player,
        legal_targets,
        returned_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("the actual returned source offers its ordinary public Aura host choice");
    };
    assert_eq!(*player, PlayerId(0));
    assert_eq!(*returned_id, source);
    assert_eq!(
        legal_targets,
        &[TargetRef::Object(hosts[0]), TargetRef::Object(hosts[1])]
    );
    assert!(
        serde_json::to_value(&runner.state().waiting_for).unwrap()["data"]
            .get("return_result_occurrence")
            .is_none()
    );
    assert_eq!(runner.state().return_result_frames.len(), 1, "the true downstream continuation, not the latched Aura host picker, keeps the caller's result alive");
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved)
        .expect("latched host/grants and the separately owned caller continuation reload");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(hosts[1])),
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&source]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(hosts[1])
    );
    assert_eq!(runner.state().players[0].life, 25);
    assert_eq!(runner.state().players[1].life, 22);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
    let next = runner.state().next_return_result_process_id;
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|definition| matches!(*definition.effect, Effect::Token { .. }))
        .expect("the ordinary installed grant really supplies its complete later activated body");
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: source,
            ability_index: index,
        })
        .unwrap();
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().players[0].life, 28, "the independent grant creates and exiles its own three-power token and reads its own fresh R11");
    assert_eq!(runner.state().players[1].life, 22);
    assert!(runner.state().next_return_result_process_id > next);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
    for foreign_payload in [1, 2] {
        let (mut bad, source, _, root) = departure_return_as_aura_runner(foreign_payload);
        let error =
            engine::types::ability::validate_instruction_result_definition(&root).unwrap_err();
        assert!(format!("{error:?}").contains("instruction-result"));
        bad.act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
        for _ in 0..8 {
            if bad.state().stack.is_empty() {
                break;
            }
            bad.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(bad.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(bad.state().players[0].life, 20);
        assert_eq!(bad.state().players[1].life, 20);
        assert!(
            bad.state()
                .zone_changes_this_turn
                .iter()
                .all(|record| record.from_zone != Some(Zone::Battlefield)),
            "persistent filter/grant refusal precedes the real caller's producer"
        );
        assert_eq!(bad.state().next_return_result_process_id, 1);
        assert!(bad.state().return_result_frames.is_empty());
        assert!(matches!(
            bad.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(bad.state().stack.is_empty());
        assert!(bad.state().resolution_stack.is_empty());
    }
}

#[test]
fn departure_token_and_emblem_later_bodies_start_fresh_after_the_creating_process_retires() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Effect, PtValue, QuantityExpr,
        StaticDefinition, TargetFilter, TriggerDefinition,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::ManaCost;
    use engine::types::triggers::TriggerMode;
    for emblem in [false, true] {
        let (mut runner, spell, subjects) =
            departure_public_program_runner(false, false, 2, |scenario| {
                let marker = scenario
                    .add_spell_to_hand(
                        PlayerId(0),
                        "Supplemental independent registered event",
                        false,
                    )
                    .with_mana_cost(ManaCost::generic(0))
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::NoOp,
                    ))
                    .id();
                let mut local = departure_selected_local_body(false, false);
                let effect = if emblem {
                    Effect::CreateEmblem {
                        statics: vec![],
                        triggers: vec![TriggerDefinition::new(TriggerMode::SpellCast)
                            .valid_card(TargetFilter::SpecificObject { id: marker })
                            .execute(local)],
                    }
                } else {
                    local.kind = AbilityKind::Activated;
                    Effect::Token {
                        name: "Supplemental independent granted artifact".into(),
                        power: PtValue::Fixed(0),
                        toughness: PtValue::Fixed(0),
                        types: vec!["Artifact".into()],
                        colors: vec![],
                        keywords: vec![],
                        tapped: false,
                        count: QuantityExpr::Fixed { value: 1 },
                        owner: TargetFilter::Controller,
                        attach_to: None,
                        enters_attacking: false,
                        supertypes: vec![],
                        static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                            .affected(TargetFilter::SelfRef)
                            .modifications(vec![ContinuousModification::GrantAbility {
                                definition: Box::new(local),
                            }])],
                        enter_with_counters: vec![],
                    }
                };
                (
                    AbilityDefinition::new(AbilityKind::Spell, effect),
                    vec![marker],
                )
            });
        // The independent marker remains in hand; primary completion includes
        // the real creation but never borrows a result into a later body.
        departure_assert_program_complete(&runner, spell, 0, 0);
        let previous = runner.state().next_return_result_process_id;
        if emblem {
            assert_eq!(
                runner
                    .state()
                    .objects
                    .values()
                    .filter(|object| object.is_emblem && object.zone == Zone::Command)
                    .count(),
                1
            );
            runner.cast(subjects[0]).commit();
        } else {
            let object = runner.state().objects.values().find(|object| object.is_token && object.name == "Supplemental independent granted artifact").expect("the real creating instruction installed the token's ordinary independent grant");
            let source = object.id;
            let index = object
                .abilities
                .iter()
                .position(|definition| matches!(*definition.effect, Effect::Token { .. }))
                .unwrap();
            runner
                .act(GameAction::ActivateAbility {
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    source_id: source,
                    ability_index: index,
                })
                .unwrap();
        }
        for _ in 0..12 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().players[0].life, 23, "the later independent token/emblem body really creates and exiles its own three-power member");
        assert_eq!(runner.state().players[1].life, 20);
        assert!(runner.state().next_return_result_process_id > previous);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
    }
}

#[test]
fn departure_retained_families_keep_legacy_this_turn_queries_without_private_result_authority() {
    // Supplemental legacy-wire fixtures start at the same real public pauses as
    // the owned-result twins. The historical quantity remains a journal query;
    // it neither imports nor manufactures the new addressed-result capability.
    fn legacy(value: &mut serde_json::Value, refs: &mut usize) {
        match value {
            serde_json::Value::Object(fields) => {
                if fields.get("type").and_then(serde_json::Value::as_str)
                    == Some("ZoneChangeAggregate")
                {
                    fields.insert(
                        "type".into(),
                        serde_json::json!("ZoneChangeAggregateThisTurn"),
                    );
                    fields.remove("source");
                    if let Some(data) = fields
                        .get_mut("data")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        data.remove("source");
                    }
                    *refs += 1;
                }
                for field in [
                    "return_result_process",
                    "expected_return_result_process",
                    "return_result_occurrence",
                    "active_return_result_occurrence",
                    "next_return_result_process_id",
                    "declares_return_result",
                ] {
                    fields.remove(field);
                }
                for child in fields.values_mut() {
                    legacy(child, refs);
                }
            }
            serde_json::Value::Array(values) => {
                for child in values {
                    legacy(child, refs);
                }
            }
            _ => {}
        }
    }
    for family in [
        DepartureRetainedFamily::Branch,
        DepartureRetainedFamily::Vote,
        DepartureRetainedFamily::Die,
        DepartureRetainedFamily::Explore,
        DepartureRetainedFamily::Clash,
        DepartureRetainedFamily::Piles,
        DepartureRetainedFamily::PilesAll,
        DepartureRetainedFamily::ZoneOpponent,
        DepartureRetainedFamily::ZoneEach,
        DepartureRetainedFamily::ZoneReciprocal,
        DepartureRetainedFamily::ScopedSearch,
        DepartureRetainedFamily::Unless,
        DepartureRetainedFamily::UnlessOneOf,
        DepartureRetainedFamily::UnlessDiscard,
        DepartureRetainedFamily::UnlessSacrifice,
        DepartureRetainedFamily::UnlessBounce,
        DepartureRetainedFamily::DiscardTail,
        DepartureRetainedFamily::SacrificeTail,
        DepartureRetainedFamily::CoinSingle,
        DepartureRetainedFamily::MultiTarget,
        DepartureRetainedFamily::CategoryCounter,
        DepartureRetainedFamily::CategoryExile,
    ] {
        let (mut runner, spell, _) = departure_retained_family_runner(family, false, false);
        assert_eq!(
            runner.state().return_result_frames.len(),
            1,
            "{family:?} reaches a nontrivial public pause"
        );
        let mut saved = serde_json::to_value(runner.state()).unwrap();
        saved
            .as_object_mut()
            .unwrap()
            .remove("return_result_frames");
        let mut refs = 0;
        legacy(&mut saved, &mut refs);
        assert!(
            refs > 0,
            "{family:?} actually contains the historical quantity"
        );
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("legacy current-turn readers require no process owner or frame");
        assert!(runner.state().return_result_frames.is_empty());
        for projected in [
            engine::game::visibility::filter_state_for_viewer(runner.state(), PlayerId(0)),
            engine::game::visibility::filter_state_for_viewer(runner.state(), PlayerId(1)),
            engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        ] {
            assert!(!serde_json::to_string(&projected)
                .unwrap()
                .contains("return_result_process"));
        }
        departure_drain_retained_public_program(&mut runner);
        let multiplier = if matches!(
            family,
            DepartureRetainedFamily::ScopedSearch | DepartureRetainedFamily::CategoryCounter
        ) {
            0
        } else if matches!(
            family,
            DepartureRetainedFamily::Vote | DepartureRetainedFamily::PilesAll
        ) {
            2
        } else {
            1
        };
        departure_assert_program_complete(&runner, spell, 5 * multiplier, 2 * multiplier);
    }
}

#[test]
fn departure_redirected_member_records_its_actual_arrival_without_joining_the_printed_exile_reduction(
) {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition,
        ReplacementMode, ReturnResultId, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    use engine::types::replacements::ReplacementEvent;
    let (mut runner, spell, sources) = departure_public_program_runner_with_setup(
        false,
        false,
        2,
        |scenario| {
            let redirect: Effect = serde_json::from_value(serde_json::json!({
                "type":"ChangeZone", "destination":"Hand", "target":{"type":"SelfRef"}
            }))
            .unwrap();
            let first = scenario
                .add_artifact_from_oracle(PlayerId(0), "Supplemental departure redirect", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::Moved)
                        .valid_card(TargetFilter::None)
                        .destination_zone(Zone::Exile)
                        .execute(AbilityDefinition::new(AbilityKind::Spell, redirect)),
                )
                .id();
            let second = scenario
                .add_artifact_from_oracle(PlayerId(0), "Supplemental departure pause", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::Moved)
                        .valid_card(TargetFilter::None)
                        .destination_zone(Zone::Exile)
                        .mode(ReplacementMode::MayCost {
                            cost: AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                            },
                            decline: Some(Box::new(AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::NoOp,
                            ))),
                        }),
                )
                .id();
            (departure_branch_reader(), vec![first, second])
        },
        |state, victims| {
            let redirect = state
                .objects
                .values()
                .find(|object| object.name == "Supplemental departure redirect")
                .unwrap()
                .id;
            let redirect = state.objects.get_mut(&redirect).unwrap();
            redirect.replacement_definitions[0].valid_card =
                Some(TargetFilter::SpecificObject { id: victims[0] });
            let pause = state
                .objects
                .values()
                .find(|object| object.name == "Supplemental departure pause")
                .unwrap()
                .id;
            let pause = state.objects.get_mut(&pause).unwrap();
            pause.replacement_definitions[0].valid_card =
                Some(TargetFilter::SpecificObject { id: victims[1] });
        },
    );
    assert_eq!(sources.len(), 2);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let publisher = runner
        .state()
        .active_change_zone_frame()
        .unwrap()
        .pending
        .as_ref()
        .unwrap();
    let (occurrence, result) = publisher.pending_return_result_producer.unwrap();
    assert_eq!(result, ReturnResultId(7));
    assert!(!runner.state().return_result_frames[&occurrence]
        .results
        .contains_key(&result));
    assert_eq!(runner.state().players[0].life, 20);
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved)
        .expect("a redirected prior member does not complete the still-parked producer");
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    let InstructionResult::ZoneChanges(records) =
        &runner.state().return_result_frames[&occurrence].results[&result]
    else {
        unreachable!()
    };
    assert_eq!(
        records.len(),
        3,
        "each actual terminal move contributes once, including the redirected arrival"
    );
    let redirected = records
        .iter()
        .filter(|record| record.to_zone == Zone::Hand)
        .collect::<Vec<_>>();
    assert_eq!(redirected.len(), 1);
    assert_eq!(redirected[0].from_zone, Some(Zone::Battlefield));
    assert_eq!(redirected[0].power, Some(3));
    assert_eq!(redirected[0].controller, PlayerId(0));
    assert_eq!(
        runner.state().objects[&redirected[0].object_id].zone,
        Zone::Hand
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.to_zone == Zone::Exile)
            .count(),
        2
    );
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved).unwrap();
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    departure_assert_program_complete(&runner, spell, 2, 2);
}

#[test]
fn departure_persistent_payload_boundaries_refuse_outer_results_before_public_root_effects() {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Comparator,
        ContinuousModification, DamageModification, Effect, ParsedCondition, PlayerFilter,
        PlayerRelation, QuantityExpr, ReplacementDefinition, StaticCondition, StaticDefinition,
        TargetFilter, TriggerConstraint, TriggerDefinition, UnlessPayModifier, UnlessPayScaling,
        ZoneChangeRecordSource,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    for row in 0..10 {
        for instruction in [false, true] {
            let Effect::GainLife {
                amount: QuantityExpr::Ref { mut qty },
                ..
            } = *departure_gain_definition(7, P0).effect
            else {
                unreachable!()
            };
            if !instruction {
                let engine::types::ability::QuantityRef::ZoneChangeAggregate { source, .. } =
                    &mut qty
                else {
                    unreachable!()
                };
                *source = ZoneChangeRecordSource::ThisTurn;
            }
            let value = QuantityExpr::Ref { qty: qty.clone() };
            let filter = TargetFilter::PlayerMatching {
                player: Box::new(PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::All,
                    attr: Box::new(qty.clone()),
                    comparator: Comparator::GE,
                    value: Box::new(QuantityExpr::Fixed { value: 0 }),
                }),
            };
            let condition = ParsedCondition::QuantityComparison {
                lhs: value.clone(),
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 0 },
            };
            let mut definition = StaticDefinition::new(StaticMode::Continuous);
            match row {
                0 => {
                    definition.condition = Some(StaticCondition::SourceMatchesFilter {
                        filter: filter.clone(),
                    })
                }
                1 => {
                    definition.condition = Some(StaticCondition::TopOfLibraryMatches {
                        filter: filter.clone(),
                    })
                }
                2 => {
                    definition.condition = Some(StaticCondition::RecipientMatchesFilter {
                        filter: filter.clone(),
                    })
                }
                3 => {
                    definition.condition = Some(StaticCondition::UnlessPay {
                        cost: ManaCost::generic(1),
                        scaling: UnlessPayScaling::PerAffectedAndQuantityRef {
                            quantity: qty.clone(),
                        },
                        defended: None,
                    })
                }
                4 => {
                    definition.mode = StaticMode::CastWithAlternativeCost {
                        cost: AbilityCost::PayLife {
                            amount: value.clone(),
                        },
                        timing_permission: None,
                        frequency: engine::types::statics::CastFrequency::Unlimited,
                    }
                }
                5 => {
                    definition.mode = StaticMode::CantHaveKeyword {
                        keyword: Keyword::Crew {
                            power: 1,
                            once_per_turn: Some(Box::new(
                                ActivationRestriction::RequiresCondition {
                                    condition: Some(condition.clone()),
                                },
                            )),
                        },
                    }
                }
                6 => {
                    definition.modifications =
                        vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                            source: TargetFilter::Any,
                            cap: Some(ActivationRestriction::RequiresCondition {
                                condition: Some(condition.clone()),
                            }),
                            kind: None,
                        }]
                }
                7 => {
                    let TargetFilter::PlayerMatching { player } = filter.clone() else {
                        unreachable!()
                    };
                    definition.per_player_condition = Some(ParsedCondition::PlayerCountAtLeast {
                        filter: *player,
                        minimum: 1,
                    });
                }
                8 => {
                    let mut trigger =
                        TriggerDefinition::new(engine::types::triggers::TriggerMode::SpellCast)
                            .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
                    trigger.constraint = Some(TriggerConstraint::NthSpellThisTurn {
                        n: 1,
                        comparator: Comparator::EQ,
                        filter: Some(filter.clone()),
                    });
                    trigger.unless_pay = Some(UnlessPayModifier {
                        cost: AbilityCost::PayLife {
                            amount: value.clone(),
                        },
                        payer: filter.clone(),
                    });
                    definition.modifications = vec![ContinuousModification::GrantTrigger {
                        trigger: Box::new(trigger),
                    }];
                }
                9 => {
                    let mut replacement = ReplacementDefinition::new(
                        engine::types::replacements::ReplacementEvent::Moved,
                    );
                    replacement.damage_modification = Some(DamageModification::Plus {
                        value: value.clone(),
                    });
                    definition.modifications = vec![ContinuousModification::GrantReplacement {
                        replacement: Box::new(replacement),
                    }];
                }
                _ => unreachable!(),
            }
            let mut persistent = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateEmblem {
                    statics: vec![definition],
                    triggers: vec![],
                },
            );
            persistent.sub_ability = Some(Box::new(departure_dual_reader()));
            let mut root = departure_exile_definition(7);
            root.sub_ability = Some(Box::new(persistent));
            let mut scenario = GameScenario::new();
            scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
            let victims = [
                scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
                scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
            ];
            let spell = scenario
                .add_spell_to_hand(P0, "Supplemental persistent-boundary public program", false)
                .with_ability_definition(root.clone())
                .with_mana_cost(ManaCost::generic(1))
                .id();
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
            );
            let mut runner = scenario.build();
            let canonical = build_resolved_from_def(&root, spell, P0);
            let mut pure = runner.state().clone();
            let before = serde_json::to_value(&pure).unwrap();
            let mut events = vec![];
            let result = resolve_ability_chain(&mut pure, &canonical, &mut events, 0);
            if instruction {
                assert!(
                    format!("{:?}", result.unwrap_err())
                        .contains("foreign instruction-result dependency"),
                    "persistent row {row}"
                );
                assert_eq!(serde_json::to_value(&pure).unwrap(), before);
                assert!(events.is_empty());
            } else {
                assert!(
                    result.is_ok(),
                    "ordinary current-turn row {row} is admitted"
                );
            }
            runner.cast(spell).resolve();
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                0,
                "the public stack paid for the real root"
            );
            for victim in victims {
                assert_eq!(
                    runner.state().objects[&victim].zone,
                    if instruction {
                        Zone::Battlefield
                    } else {
                        Zone::Exile
                    }
                );
            }
            assert_eq!(
                runner
                    .state()
                    .objects
                    .values()
                    .filter(|object| object.is_emblem)
                    .count(),
                if instruction { 0 } else { 1 }
            );
            assert_eq!(
                runner.state().next_return_result_process_id,
                if instruction { 1 } else { 2 }
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            departure_assert_program_complete(
                &runner,
                spell,
                if instruction { 0 } else { 5 },
                if instruction { 0 } else { 2 },
            );
        }
    }
}

#[test]
fn departure_saved_inner_local_pause_cannot_import_the_outer_result_even_when_both_frames_are_complete(
) {
    use engine::types::ability::{Effect, QuantityExpr, ReturnResultId};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |scenario| {
            (
                departure_selected_family_definition(scenario, 0, true, false),
                vec![],
            )
        });
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        let WaitingFor::ChooseOneOfBranch {
            return_result_occurrence: Some(inner),
            expected_return_result_process: Some(inner_process),
            branches,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the local producer must reach its own real inner prompt")
        };
        let inner = *inner;
        assert_eq!(runner.state().return_result_frames.len(), 2);
        assert_eq!(
            runner.state().return_result_frames[&inner].process,
            Some(*inner_process)
        );
        assert_eq!(branches.len(), 2);
        assert!(
            matches!(&runner.state().return_result_frames[&inner].results[&ReturnResultId(11)], InstructionResult::ZoneChanges(records) if records.len() == 1 && records[0].power == Some(3))
        );
        let (&outer, outer_frame) = runner
            .state()
            .return_result_frames
            .iter()
            .find(|(id, _)| **id != inner)
            .unwrap();
        assert_ne!(outer_frame.process, Some(*inner_process));
        assert!(outer_frame.results.contains_key(&ReturnResultId(7)));
        assert!(!runner.state().return_result_frames[&inner]
            .results
            .contains_key(&ReturnResultId(7)));
        let mut hostile = runner.state().clone();
        let WaitingFor::ChooseOneOfBranch { branches, .. } = &mut hostile.waiting_for else {
            unreachable!()
        };
        let Effect::GainLife { amount, .. } = branches[1].effect.as_mut() else {
            unreachable!()
        };
        let Effect::GainLife {
            amount: imported, ..
        } = *departure_gain_definition(7, PlayerId(0)).effect
        else {
            unreachable!()
        };
        *amount = QuantityExpr::Sum {
            exprs: vec![amount.clone(), imported],
        };
        let before = serde_json::to_value(&hostile).unwrap();
        assert!(
            serde_json::from_value::<GameState>(before.clone()).is_err(),
            "the complete foreign outer frame is not part of the local process"
        );
        let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
        assert!(bad.act(GameAction::ChooseBranch { index: 1 }).is_err());
        assert_eq!(
            serde_json::to_value(bad.state()).unwrap(),
            before,
            "reject before consuming the selected branch or either result/frame"
        );
        assert!(bad.state().return_result_frames.contains_key(&outer));
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect(
            "the exact local-three plus outer-five program reloads at its real inner pause",
        );
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        departure_assert_program_complete(
            &runner,
            spell,
            3 + if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

#[test]
fn oversimplify_two_paid_same_name_sources_exclude_prior_departures_and_retired_first_process() {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let unrelated = scenario
        .add_creature(P0, "Supplemental prior nine", 9, 9)
        .id();
    let victims = [
        scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
        scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
        scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
    ];
    let prior_effect: Effect = serde_json::from_value(serde_json::json!({"type":"ChangeZone", "origin":"Battlefield", "destination":"Exile", "target":{"type":"SpecificObject","id":unrelated}})).unwrap();
    let prior = scenario
        .add_spell_to_hand(P0, "Supplemental paid unrelated exile", false)
        .with_ability_definition(AbilityDefinition::new(AbilityKind::Spell, prior_effect))
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let wipes = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Oversimplify", false, OVERSIMPLIFY_SOURCE)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            })
            .id()
    });
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Black,
            ManaType::Green,
            ManaType::Blue,
            ManaType::Green,
            ManaType::Blue,
        ]
        .into_iter()
        .chain(std::iter::repeat_n(ManaType::Colorless, 6))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(prior).resolve();
    assert_eq!(runner.state().objects[&unrelated].zone, Zone::Exile);
    assert_eq!(runner.state().players[0].mana_pool.total(), 10);
    departure_assert_program_complete(&runner, prior, 0, 0);
    let mut first_tokens = vec![];
    let mut next_process = runner.state().next_return_result_process_id;
    for (index, spell) in wipes.into_iter().enumerate() {
        let outcome = runner.cast(spell).resolve();
        let tokens: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| {
                if let GameEvent::TokenCreated { object_id, .. } = event {
                    Some(*object_id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            tokens.len(),
            2,
            "each printed instruction creates one token for each player exactly once"
        );
        for (player, count) in [(P0, 5), (P1, 2)] {
            let token = tokens
                .iter()
                .map(|id| &outcome.state().objects[id])
                .find(|token| token.controller == player)
                .unwrap();
            assert_eq!(token.zone, Zone::Battlefield);
            assert_eq!(token.owner, player);
            assert_eq!(token.counters.get(&CounterType::Plus1Plus1), Some(&count));
            assert_eq!((token.base_power, token.base_toughness), (Some(0), Some(0)));
            assert!(
                token.color.contains(&ManaColor::Green) && token.color.contains(&ManaColor::Blue)
            );
        }
        if index == 0 {
            for victim in victims {
                assert_eq!(outcome.state().objects[&victim].zone, Zone::Exile);
            }
            first_tokens = tokens;
        } else {
            assert!(tokens.iter().all(|id| !first_tokens.contains(id)));
            for token in &first_tokens {
                assert!(!outcome.state().battlefield.contains(token));
                assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::ZoneChanged { record, .. } if record.object_id == *token && record.from_zone == Some(Zone::Battlefield) && record.to_zone == Zone::Exile)));
            }
        }
        assert!(outcome.state().next_return_result_process_id > next_process);
        next_process = outcome.state().next_return_result_process_id;
        assert_eq!(
            outcome.state().players[0].mana_pool.total(),
            if index == 0 { 5 } else { 0 }
        );
        departure_assert_program_complete(&runner, spell, 0, 0);
    }
    assert_eq!(
        runner.state().objects[&wipes[0]].name,
        runner.state().objects[&wipes[1]].name
    );
    assert_ne!(wipes[0], wipes[1]);
}

#[test]
fn oversimplify_paid_empty_battlefield_publishes_present_empty_and_creates_both_zero_counter_tokens(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oversimplify", false, OVERSIMPLIFY_SOURCE)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let births: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| {
            if let GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        births.len(),
        2,
        "present-empty is read by both real printed per-player token instructions"
    );
    for id in births {
        assert!(!outcome.state().battlefield.contains(&id));
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::ZoneChanged { record, .. } if record.object_id == id && record.from_zone == Some(Zone::Battlefield) && record.to_zone == Zone::Graveyard && record.power == Some(0) && record.toughness == Some(0))));
        assert!(!outcome.events().iter().any(
            |event| matches!(event, GameEvent::CounterAdded { object_id, .. } if *object_id == id)
        ));
    }
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    assert_eq!(outcome.state().next_return_result_process_id, 2);
    departure_assert_program_complete(&runner, spell, 0, 0);
}

#[test]
fn departure_repeated_local_three_owns_each_inner_thumb_pause_and_rejects_saved_outer_plus_local_sum(
) {
    use engine::game::scenario::P0;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReturnResultId, TargetFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, WaitingFor};
    for empty in [false, true] {
        let (mut runner, spell, _) = departure_public_program_runner(false, empty, 2, |scenario| {
            scenario.add_real_card(
                P0,
                "Krark's Thumb",
                Zone::Battlefield,
                super::support::shared_card_db().expect("pinned complete Thumb is required"),
            );
            let mut local = departure_selected_local_body(false, false);
            let producer = local.sub_ability.as_deref_mut().unwrap();
            let read = *producer.sub_ability.take().unwrap();
            producer.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    flips: engine::types::ability::CoinFlipRun::single(),
                    win_effect: Some(Box::new(read.clone())),
                    lose_effect: Some(Box::new(read)),
                    flipper: TargetFilter::Controller,
                },
            )));
            local.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
            let mut choice = departure_branch_reader();
            let Effect::ChooseOneOf { branches, .. } = choice.effect.as_mut() else {
                unreachable!()
            };
            branches[1] = local;
            choice.sub_ability = Some(Box::new(departure_dual_reader()));
            (choice, vec![])
        });
        let (&outer, frame) = runner.state().return_result_frames.iter().next().unwrap();
        let outer_process = frame.process;
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        let mut previous = None;
        for iteration in 0..2 {
            let WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } = &runner.state().waiting_for
            else {
                panic!("each whole local token/producer process must reach its real Thumb pause")
            };
            assert_eq!(*player, P0);
            assert_eq!(results.len(), 2);
            assert_eq!(*keep_count, 1);
            let pending = runner.state().active_coin_flip_frame().unwrap();
            let inner = pending.return_result_occurrence.unwrap();
            let process = pending.expected_return_result_process.unwrap();
            assert_ne!(inner, outer);
            assert_ne!(Some(process), outer_process);
            assert_eq!(runner.state().return_result_frames.len(), 2);
            assert_eq!(
                runner.state().return_result_frames[&inner].process,
                Some(process)
            );
            assert!(
                matches!(&runner.state().return_result_frames[&inner].results[&ReturnResultId(11)], InstructionResult::ZoneChanges(records) if records.len() == 1 && records[0].power == Some(3))
            );
            assert!(
                matches!(&runner.state().return_result_frames[&outer].results[&ReturnResultId(7)], InstructionResult::ZoneChanges(records) if records.len() == if empty { 0 } else { 3 })
            );
            assert_eq!(
                runner.state().players[0].life,
                20 + iteration * 3,
                "outer five has not run before the last local process completes"
            );
            assert_eq!(runner.state().players[1].life, 20);
            if let Some((old_o, old_p)) = previous {
                assert_ne!(old_o, inner);
                assert_ne!(old_p, process);
                assert!(!runner.state().return_result_frames.contains_key(&old_o));
            }
            previous = Some((inner, process));
            let mut hostile = runner.state().clone();
            let pending = hostile.active_coin_flip_frame_mut().unwrap();
            for definition in [&mut pending.win_effect, &mut pending.lose_effect]
                .into_iter()
                .flatten()
            {
                let Effect::GainLife { amount, .. } = definition.effect.as_mut() else {
                    unreachable!()
                };
                let Effect::GainLife {
                    amount: outer_read, ..
                } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                *amount = QuantityExpr::Sum {
                    exprs: vec![amount.clone(), outer_read],
                };
            }
            let before = serde_json::to_value(&hostile).unwrap();
            assert!(serde_json::from_value::<GameState>(before.clone()).is_err(), "a complete outer frame cannot join the current local process at its actual keep response");
            let mut bad = engine::game::scenario::GameRunner::from_state(hostile);
            assert!(bad
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1]
                })
                .is_err());
            assert_eq!(serde_json::to_value(bad.state()).unwrap(), before, "reject before choosing a flip, emitting a kept event, evaluating Sum, or consuming either frame");
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved)
                .expect("the actual local Thumb owner and separate outer tail both reload");
            let result = runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::CoinFlipped { .. }))
                    .count(),
                1
            );
        }
        departure_assert_program_complete(
            &runner,
            spell,
            6 + if empty { 0 } else { 5 },
            if empty { 0 } else { 2 },
        );
    }
}

#[test]
fn departure_mana_spend_grant_refuses_outer_import_and_runs_its_complete_local_process_after_spending(
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction, TargetFilter,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaColor, ManaCost, ManaSpellGrant, ManaType, ManaUnit};
    for foreign_import in [false, true] {
        let later = if foreign_import {
            departure_gain_definition(7, P0)
        } else {
            departure_selected_local_body(false, false)
        };
        let mana = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![ManaSpellGrant::TriggerOnSpend {
                    filter: TargetFilter::Any,
                    ability: Box::new(later),
                }],
                expiry: None,
                target: None,
            },
        );
        let mut root = departure_exile_definition(7);
        root.sub_ability = Some(Box::new(mana));
        let mut scenario = GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let victims = [
            scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
            scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
            scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
        ];
        let source = scenario
            .add_spell_to_hand(P0, "Supplemental canonical mana-spend grant", false)
            .with_ability_definition(root.clone())
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let marker = scenario
            .add_spell_to_hand(P0, "Supplemental paid grant trigger marker", false)
            .with_ability_definition(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        let canonical = engine::types::ability::validate_instruction_result_definition(&root);
        if foreign_import {
            assert!(format!("{:?}", canonical.unwrap_err())
                .contains("foreign instruction-result dependency"));
        } else {
            assert!(canonical.is_ok());
        }
        runner.cast(source).resolve();
        for victim in victims {
            assert_eq!(
                runner.state().objects[&victim].zone,
                if foreign_import {
                    Zone::Battlefield
                } else {
                    Zone::Exile
                }
            );
        }
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            usize::from(!foreign_import)
        );
        assert_eq!(
            runner.state().next_return_result_process_id,
            if foreign_import { 1 } else { 2 }
        );
        departure_assert_program_complete(&runner, source, 0, 0);
        if !foreign_import {
            let unit = runner.state().players[0].mana_pool.units().next().unwrap();
            assert_eq!(unit.color, ManaType::Green);
            assert_eq!(unit.source_id, source);
            assert!(!serde_json::to_string(&unit.grants)
                .unwrap()
                .contains("return_result_process"));
            let previous = runner.state().next_return_result_process_id;
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).expect("the unbound complete later mana-spend body persists after the creating frame retires");
            runner.cast(marker).resolve();
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                0,
                "the later paid spell spends the actual granted unit"
            );
            assert!(runner.state().next_return_result_process_id > previous);
            departure_assert_program_complete(&runner, marker, 3, 0);
        }
    }
}

// Supplemental typed compositions: the predicate is the sole addressed read.
fn departure_hidden_predicate_runner(
    row: u8,
    foreign: bool,
    empty: bool,
    source_mode: u8,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    Vec<engine::types::identifiers::ObjectId>,
) {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::mana::{ManaColor, ManaCost};
    departure_public_program_runner(foreign, empty, 2, |scenario| {
        let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, P0).effect else {
            unreachable!()
        };
        if source_mode == 1 {
            let QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate { source, .. },
            } = &mut amount
            else {
                unreachable!()
            };
            *source = ZoneChangeRecordSource::ThisTurn;
        } else if source_mode == 2 {
            amount = QuantityExpr::Fixed { value: 5 };
        }
        let mut typed = TypedFilter::default();
        typed.properties.push(FilterProp::Cmc {
            comparator: if row == 2 {
                Comparator::EQ
            } else {
                Comparator::LT
            },
            value: amount,
        });
        let predicate = TargetFilter::Typed(typed);
        let (effect, subjects) = match row {
            0 => {
                let subjects = [
                    ManaColor::Red,
                    ManaColor::Red,
                    ManaColor::Green,
                    ManaColor::Green,
                ]
                .iter()
                .enumerate()
                .map(|(index, color)| {
                    scenario
                        .add_artifact_from_oracle(
                            P0,
                            &format!("Supplemental filter category {index}"),
                            "",
                        )
                        .with_mana_cost(ManaCost::generic(2))
                        .with_color(vec![*color])
                        .id()
                })
                .collect::<Vec<_>>();
                (
                    Effect::ForEachCategory {
                        category: IterationCategory::Color,
                        chooser: Chooser::Controller,
                        action: ForEachCategoryAction::PutCounter {
                            target: predicate,
                            counter_type: CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    },
                    subjects,
                )
            }
            1 => {
                let subjects = (0..2)
                    .map(|index| {
                        scenario
                            .add_artifact_from_oracle(
                                P0,
                                &format!("Supplemental filter vote {index}"),
                                "",
                            )
                            .with_mana_cost(ManaCost::generic(2 + index))
                            .id()
                    })
                    .collect::<Vec<_>>();
                (
                    Effect::Vote {
                        choices: vec![],
                        per_choice_effect: vec![],
                        starting_with: ControllerRef::You,
                        voter_scope: VoterScope::AllPlayers,
                        tally_mode: VoteTally::TopVotes {
                            tie: TieResolution::AllTied,
                        },
                        subject: VoteSubject::Objects {
                            candidate_filter: predicate,
                            outcome_template: Box::new(AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::NoOp,
                            )),
                        },
                        visibility: VoteVisibility::Open,
                    },
                    subjects,
                )
            }
            2 | 3 => {
                let hit = scenario
                    .add_land_to_library_top(P0, "Supplemental predicate library hit")
                    .with_mana_cost(ManaCost::generic(if row == 2 {
                        if foreign {
                            7
                        } else {
                            5
                        }
                    } else {
                        3
                    }))
                    .id();
                if row == 2 && source_mode == 3 {
                    let replacement = ReplacementDefinition::new(
                        engine::types::replacements::ReplacementEvent::Moved,
                    )
                    .valid_card(TargetFilter::SpecificObject { id: hit })
                    .destination_zone(Zone::Exile)
                    .mode(ReplacementMode::MayCost {
                        cost: AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        decline: Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::NoOp,
                        ))),
                    });
                    scenario
                        .add_artifact_from_oracle(P0, "Supplemental until delivery pause", "")
                        .with_replacement_definition(replacement);
                }
                let effect = if row == 2 {
                    Effect::ExileFromTopUntil {
                        player: TargetFilter::Controller,
                        until: UntilCondition::NextMatches {
                            filter: TargetFilter::And {
                                filters: vec![TargetFilter::SpecificObject { id: hit }, predicate],
                            },
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    }
                } else {
                    Effect::RevealUntil {
                        player: TargetFilter::Controller,
                        filter: TargetFilter::SpecificObject { id: hit },
                        count: QuantityExpr::Fixed { value: 1 },
                        matched_disposition: RevealUntilDisposition::KeepEach,
                        kept_destination: Zone::Hand,
                        rest_destination: Zone::Graveyard,
                        rest_order: DigRestOrder::Preserve,
                        enter_tapped: Default::default(),
                        enters_attacking: false,
                        kept_optional_to: None,
                        enters_under: None,
                        kept_destination_if: Some((Box::new(predicate), Zone::Battlefield)),
                    }
                };
                (effect, vec![hit])
            }
            _ => unreachable!(),
        };
        let mut reader = AbilityDefinition::new(AbilityKind::Spell, effect);
        if row >= 2 {
            reader.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    player: TargetFilter::Controller,
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            )));
        }
        (
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![
                        AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                        reader,
                    ],
                    kind: ChooseOneOfKind::Ordinary,
                },
            ),
            subjects,
        )
    })
}

#[test]
fn departure_five_hidden_predicates_are_the_only_read_and_category_retains_public_custody() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for row in 0..4 {
        let (mut foreign, foreign_spell, _) =
            departure_hidden_predicate_runner(row, true, false, 0);
        let (&foreign_o, foreign_frame) =
            foreign.state().return_result_frames.iter().next().unwrap();
        let foreign_frame = foreign_frame.clone();
        // The foreign frame is from a real completed producer and actual public pause.
        foreign.act(GameAction::ChooseBranch { index: 0 }).unwrap();
        departure_assert_program_complete(&foreign, foreign_spell, 0, 0);
        for mode in 0..3 {
            for empty in [false, true] {
                let (mut runner, spell, subjects) =
                    departure_hidden_predicate_runner(row, false, empty, mode);
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ChooseOneOfBranch {
                        player: PlayerId(0),
                        ..
                    }
                ));
                if mode == 0 {
                    departure_assert_retained_hostile_reads(
                        &runner,
                        DepartureRetainedFamily::Branch,
                        foreign_o,
                        foreign_frame.clone(),
                    );
                }
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved)
                    .expect("filter-only selected import survives reload");
                runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
                let eligible = !empty || mode == 2;
                match row {
                    0 => {
                        for member in 0..if eligible { 2 } else { 0 } {
                            let WaitingFor::ChooseFromZoneChoice {
                                player,
                                cards,
                                count,
                                ..
                            } = &runner.state().waiting_for
                            else {
                                panic!("two live category members must each pause");
                            };
                            assert_eq!(*player, PlayerId(0));
                            assert_eq!(*count, 1);
                            assert_eq!(cards.len(), 2);
                            let chosen = cards[1];
                            let pending = runner.state().active_per_category_zone_choice().unwrap();
                            if mode == 0 {
                                assert!(
                                    pending.return_result_occurrence.is_some(),
                                    "remaining predicates alone keep O live"
                                );
                                departure_assert_retained_hostile_reads(
                                    &runner,
                                    DepartureRetainedFamily::CategoryCounter,
                                    foreign_o,
                                    foreign_frame.clone(),
                                );
                            }
                            assert!(
                                runner
                                    .state()
                                    .resolution_stack
                                    .iter()
                                    .find_map(|frame| match frame {
                                        engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => Some(&frame.pending),
                                        _ => None,
                                    })
                                    .is_none(),
                                "category has no outer executable tail"
                            );
                            let saved = serde_json::to_value(runner.state()).unwrap();
                            *runner.state_mut() = serde_json::from_value(saved)
                                .expect("remaining category predicate survives each re-pause");
                            runner
                                .act(GameAction::SelectCards {
                                    cards: vec![chosen],
                                })
                                .unwrap();
                            assert_eq!(
                                runner.state().objects[&chosen].counters[&CounterType::Plus1Plus1],
                                1
                            );
                            assert!(member < 2);
                        }
                        assert_eq!(
                            subjects
                                .iter()
                                .map(|id| runner.state().objects[id]
                                    .counters
                                    .get(&CounterType::Plus1Plus1)
                                    .copied()
                                    .unwrap_or(0))
                                .sum::<u32>(),
                            if eligible { 2 } else { 0 }
                        );
                    }
                    1 if eligible => {
                        for seat in 0..2 {
                            let WaitingFor::VoteChoice {
                                player,
                                candidate_objects,
                                return_result_occurrence,
                                ..
                            } = &runner.state().waiting_for
                            else {
                                panic!("enumerated object candidates reach public vote");
                            };
                            assert_eq!(*player, PlayerId(seat));
                            assert_eq!(candidate_objects.len(), 2);
                            assert!(
                                return_result_occurrence.is_none(),
                                "candidate IDs are latched and outcome has no live read"
                            );
                            let saved = serde_json::to_value(runner.state()).unwrap();
                            *runner.state_mut() = serde_json::from_value(saved).unwrap();
                            runner
                                .act(GameAction::SubmitVoteCandidate { candidate_index: 1 })
                                .unwrap();
                        }
                    }
                    2 => {
                        assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Exile);
                        assert!(runner.state().library_of(PlayerId(0)).is_empty(), "the buried matching card or known-empty scan reaches the actual reader");
                    }
                    3 => assert_eq!(
                        runner.state().objects[&subjects[0]].zone,
                        if eligible {
                            Zone::Battlefield
                        } else {
                            Zone::Hand
                        }
                    ),
                    _ => {}
                }
                departure_assert_program_complete(
                    &runner,
                    spell,
                    if row == 0 || row == 1 || (row == 2 && empty && mode != 2) {
                        0
                    } else {
                        1
                    },
                    0,
                );
            }
        }
    }
}

#[test]
fn departure_opponent_restriction_refuses_before_root_and_keeps_this_turn_fixed_and_nonreader_guesses(
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for mode in 0..4 {
        let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, P0).effect else {
            unreachable!()
        };
        if mode == 1 {
            let QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate { source, .. },
            } = &mut amount
            else {
                unreachable!()
            };
            *source = ZoneChangeRecordSource::ThisTurn;
        } else if mode == 2 {
            amount = QuantityExpr::Fixed { value: 5 };
        }
        let choice_type = ChoiceType::Opponent {
            restriction: Some(Box::new(PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::Opponent,
                attr: Box::new(QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                }),
                comparator: Comparator::GE,
                value: Box::new(amount),
            })),
            distinctness: PlayerChoiceDistinctness::Independent,
        };
        let effect = if mode == 3 {
            Effect::OpponentGuess {
                guesser: ControllerRef::Opponent,
                subject: Box::new(GuessSubject::CommittedChoice { choice_type }),
            }
        } else {
            Effect::Choose {
                choice_type,
                persist: false,
                selection: TargetSelectionMode::Chosen,
            }
        };
        let mut root = departure_exile_definition(7);
        let mut body = AbilityDefinition::new(AbilityKind::Spell, effect);
        let mut tail = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                player: TargetFilter::Controller,
                amount: QuantityExpr::Fixed { value: 1 },
            },
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        body.sub_ability = Some(Box::new(tail));
        root.sub_ability = Some(Box::new(body));
        let mut scenario = GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let victim = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental opponent predicate", false)
            .with_ability_definition(root.clone())
            .id();
        for player in [P0, P1] {
            for _ in 0..5 {
                scenario.add_card_to_library_top(player, "Library witness");
            }
        }
        let mut runner = scenario.build();
        if mode == 0 {
            let mut state = runner.state().clone();
            let before = serde_json::to_value(&state).unwrap();
            let mut events = Vec::new();
            let resolved = build_resolved_from_def(&root, spell, P0);
            let error = resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap_err();
            assert!(
                format!("{error:?}").contains("opponent restriction requires executable context")
            );
            assert_eq!(serde_json::to_value(&state).unwrap(), before);
            assert!(events.is_empty());
        }
        runner.cast(spell).commit();
        for _ in 0..8 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                && !runner.state().stack.is_empty()
            {
                runner.act(GameAction::PassPriority).unwrap();
            } else {
                break;
            }
        }
        if mode == 0 {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
            assert_eq!(runner.state().next_return_result_occurrence_id, 1);
            assert_eq!(runner.state().next_return_result_process_id, 1);
            departure_assert_program_complete(&runner, spell, 0, 0);
        } else {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
            if mode != 3 {
                let WaitingFor::NamedChoice {
                    player, options, ..
                } = &runner.state().waiting_for
                else {
                    panic!("ordinary source-only predicate remains a functioning public opponent choice");
                };
                assert_eq!(*player, P0);
                assert_eq!(options, &vec!["1".to_string()]);
                runner
                    .act(GameAction::ChooseOption { choice: "1".into() })
                    .unwrap();
            }
            departure_assert_program_complete(&runner, spell, 1, 0);
        }
    }
}

#[test]
fn departure_raw_replacements_refuse_outer_imports_before_producers_and_install_independent_controls(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for planeswalk in [false, true] {
        for mode in 0..4 {
            let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, P0).effect
            else {
                unreachable!()
            };
            if mode == 1 {
                let QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { source, .. },
                } = &mut amount
                else {
                    unreachable!()
                };
                *source = ZoneChangeRecordSource::ThisTurn;
            } else if mode == 2 {
                amount = QuantityExpr::Fixed { value: 5 };
            }
            let replacement_effect = if mode == 3 {
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![
                        AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                        departure_selected_local_body(false, false),
                    ],
                    kind: ChooseOneOfKind::Ordinary,
                }
            } else {
                Effect::GainLife {
                    player: TargetFilter::Controller,
                    amount,
                }
            };
            let effect = if planeswalk {
                Effect::CreatePlaneswalkReplacement {
                    replacement_effect: Box::new(replacement_effect),
                }
            } else {
                Effect::CreateDrawReplacement {
                    replacement_effect: Box::new(replacement_effect),
                }
            };
            let mut root = departure_exile_definition(7);
            root.sub_ability = Some(Box::new(AbilityDefinition::new(AbilityKind::Spell, effect)));
            let mut scenario = GameScenario::new();
            scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
            let victim = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
            let spell = scenario
                .add_spell_to_hand(P0, "Supplemental raw replacement carrier", false)
                .with_ability_definition(root.clone())
                .id();
            for player in [P0, PlayerId(1)] {
                for _ in 0..8 {
                    scenario.add_card_to_library_top(player, "Library witness");
                }
            }
            let mut runner = scenario.build();
            if mode == 0 {
                let mut state = runner.state().clone();
                let before = serde_json::to_value(&state).unwrap();
                let mut events = Vec::new();
                let error = resolve_ability_chain(
                    &mut state,
                    &build_resolved_from_def(&root, spell, P0),
                    &mut events,
                    0,
                )
                .unwrap_err();
                assert!(format!("{error:?}").contains("foreign instruction-result dependency"));
                assert_eq!(serde_json::to_value(&state).unwrap(), before);
                assert!(events.is_empty());
            }
            runner.cast(spell).resolve();
            departure_assert_program_complete(&runner, spell, 0, 0);
            if mode == 0 {
                assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
                assert!(runner.state().pending_damage_replacements.is_empty());
                assert_eq!(runner.state().next_return_result_process_id, 1);
            } else {
                assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
                let substitute = runner
                    .state()
                    .pending_damage_replacements
                    .last()
                    .expect("the actual carrier installed its later raw executable")
                    .runtime_execute
                    .as_deref()
                    .unwrap()
                    .clone();
                assert!(substitute.return_result_process.is_none());
                // Supplemental independent invocation of the actually installed body;
                // public selection and priority complete its nested local process.
                let mut events = Vec::new();
                // Supplemental installed-body invocation; actual event-driven installations have separate production regressions.
                resolve_ability_chain(runner.state_mut(), &substitute, &mut events, 0).unwrap();
                if mode == 3 {
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::ChooseOneOfBranch { player: P0, .. }
                    ));
                    let saved = serde_json::to_value(runner.state()).unwrap();
                    *runner.state_mut() = serde_json::from_value(saved).unwrap();
                    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
                }
                assert_eq!(
                    runner.state().players[0].life,
                    20 + if mode == 1 || mode == 3 { 3 } else { 5 }
                );
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(runner.state().stack.is_empty());
                assert!(runner.state().resolution_stack.is_empty());
                assert!(runner.state().return_result_frames.is_empty());
            }
        }
    }
}

#[test]
fn departure_epic_snapshot_validates_locally_and_public_copy_gets_fresh_process() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ReturnResultProcessId, StackEntryKind, WaitingFor};
    use engine::types::identifiers::ObjectId;
    for mode in 0..3 {
        let (mut runner, spell, _) = departure_public_program_runner(false, false, 2, |_| {
            let definition = if mode == 0 {
                departure_selected_local_body(false, false)
            } else {
                let mut definition = departure_gain_definition(7, P0);
                let Effect::GainLife { amount, .. } = definition.effect.as_mut() else {
                    unreachable!()
                };
                if mode == 1 {
                    let QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeAggregate { source, .. },
                    } = amount
                    else {
                        unreachable!()
                    };
                    *source = ZoneChangeRecordSource::ThisTurn;
                } else {
                    *amount = QuantityExpr::Fixed { value: 5 };
                }
                definition
            };
            let mut snapshot = build_resolved_from_def(&definition, ObjectId(0), P0);
            snapshot.return_result_process = Some(ReturnResultProcessId(901));
            (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseOneOf {
                        chooser: PlayerFilter::Controller,
                        branches: vec![
                            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                            AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::EpicCopy {
                                    spell: Box::new(snapshot),
                                },
                            ),
                        ],
                        kind: ChooseOneOfKind::Ordinary,
                    },
                ),
                vec![],
            )
        });
        let WaitingFor::ChooseOneOfBranch {
            player, branches, ..
        } = &runner.state().waiting_for
        else {
            panic!("retain the raw Epic snapshot before consumption/copy-time clearing");
        };
        assert_eq!(*player, P0);
        let Effect::EpicCopy { spell: snapshot } = branches[1].effect.as_ref() else {
            unreachable!()
        };
        assert_eq!(
            snapshot.return_result_process,
            Some(ReturnResultProcessId(901))
        );
        departure_assert_private_result_views(runner.state());
        let retained = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(retained.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(runner.state()).unwrap(),
            retained,
            "authoritative saves retain the raw snapshot identity"
        );
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        let copy = runner
            .state()
            .stack
            .back()
            .expect("Epic creates an actual independent stack copy");
        let copy_id = copy.id;
        assert_ne!(copy_id, spell);
        assert!(runner.state().objects[&copy_id].is_token);
        let StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &copy.kind
        else {
            unreachable!()
        };
        assert!(
            ability.return_result_process.is_none(),
            "Epic source-rebinding clears stale snapshot authority before stack storage"
        );
        assert!(ability
            .sub_ability
            .as_deref()
            .is_none_or(|child| child.return_result_process.is_none()));
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved)
            .expect("independent copy is structurally valid before local publication");
        for _ in 0..8 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner.act(GameAction::PassPriority).unwrap();
        }
        departure_assert_program_complete(&runner, spell, if mode == 0 { 3 } else { 5 }, 0);
        assert!(runner.state().next_return_result_process_id > 1);
    }
}

#[test]
fn departure_next_matches_filter_survives_actual_replacement_delivery_and_reload() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::ReturnResultId;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{InstructionResult, RevealedInstructionResult, WaitingFor};
    let (mut foreign, foreign_spell, _) = departure_hidden_predicate_runner(2, true, false, 0);
    let (&foreign_o, foreign_frame) = foreign.state().return_result_frames.iter().next().unwrap();
    let foreign_frame = foreign_frame.clone();
    foreign.act(GameAction::ChooseBranch { index: 0 }).unwrap();
    departure_assert_program_complete(&foreign, foreign_spell, 0, 0);
    for empty in [false, true] {
        let (mut runner, spell, subjects) = departure_hidden_predicate_runner(2, false, empty, 3);
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        let WaitingFor::ReplacementChoice { player, .. } = &runner.state().waiting_for else {
            panic!("the matching library card pauses in its actual delivery");
        };
        assert_eq!(*player, PlayerId(0));
        assert!(runner.state().pending_exile_from_top_until.is_some());
        let continuation = runner
            .state()
            .resolution_stack
            .iter()
            .filter_map(|frame| match frame {
                engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                    Some(&frame.pending)
                }
                _ => None,
            })
            .find(|pending| {
                matches!(
                    pending.chain.effect,
                    engine::types::ability::Effect::ExileFromTopUntil { .. }
                )
            })
            .expect("the actual unresolved predicate is retained for resumed delivery");
        assert!(continuation.return_result_occurrence.is_some());
        assert_eq!(
            runner.state().return_result_frames[&continuation.return_result_occurrence.unwrap()]
                .process,
            continuation.chain.return_result_process
        );
        let original = continuation.return_result_occurrence.unwrap();
        assert_ne!(
            foreign_frame.process,
            continuation.chain.return_result_process
        );
        for fault in 0..if empty { 6 } else { 7 } {
            let mut hostile = runner.state().clone();
            match fault {
                0 => {
                    hostile.return_result_frames.remove(&original);
                    hostile
                        .return_result_frames
                        .insert(foreign_o, foreign_frame.clone());
                    hostile.next_return_result_occurrence_id = foreign_o.0 + 1;
                    hostile.next_return_result_process_id = foreign_frame.process.unwrap().0 + 1;
                    let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                    departure_rewrite_lookup(
                        &mut stack,
                        &serde_json::to_value(original).unwrap(),
                        &serde_json::to_value(foreign_o).unwrap(),
                    );
                    hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                }
                1 => hostile.return_result_frames.clear(),
                2 => {
                    hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .remove(&ReturnResultId(7));
                }
                3 => {
                    hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .insert(
                            ReturnResultId(7),
                            InstructionResult::RevealedCards(RevealedInstructionResult {
                                revealed_cards: vec![],
                            }),
                        );
                }
                4 => {
                    let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                    assert!(departure_clear_continuation_process(&mut stack) > 0);
                    hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                }
                5 => {
                    let mut stack = serde_json::to_value(&hostile.resolution_stack).unwrap();
                    departure_rewrite_lookup(
                        &mut stack,
                        &serde_json::to_value(original).unwrap(),
                        &serde_json::Value::Null,
                    );
                    hostile.resolution_stack = serde_json::from_value(stack).unwrap();
                }
                6 => {
                    let InstructionResult::ZoneChanges(records) = hostile
                        .return_result_frames
                        .get_mut(&original)
                        .unwrap()
                        .results
                        .get_mut(&ReturnResultId(7))
                        .unwrap()
                    else {
                        unreachable!()
                    };
                    records[0].trigger_source_context = None;
                }
                _ => unreachable!(),
            }
            let error =
                serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap())
                    .unwrap_err()
                    .to_string();
            if fault == 0 || fault == 4 {
                assert!(
                    error.contains("instruction-result process owner mismatch"),
                    "resumed filter reload: {error}"
                );
            }
            let mut reached = GameRunner::from_state(hostile);
            let before = serde_json::to_value(reached.state()).unwrap();
            let error = reached
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap_err()
                .to_string();
            if fault == 0 || fault == 4 {
                assert!(
                    error.contains("instruction-result process owner mismatch"),
                    "resumed filter response: {error}"
                );
            }
            assert_eq!(
                serde_json::to_value(reached.state()).unwrap(),
                before,
                "invalid resumed filter cannot deliver or consume the replacement"
            );
        }
        departure_assert_private_result_views(runner.state());
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).expect(
            "the live per-card filter keeps its exact process through replacement delivery",
        );
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
        assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Exile);
        assert!(runner.state().pending_exile_from_top_until.is_none());
        departure_assert_program_complete(&runner, spell, if empty { 0 } else { 1 }, 0);
    }
}

#[test]
fn departure_epic_import_is_refused_before_the_outer_producer_can_execute() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let victim = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
    let mut root = departure_exile_definition(7);
    let snapshot = build_resolved_from_def(&departure_gain_definition(7, P0), victim, P0);
    root.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EpicCopy {
            spell: Box::new(snapshot),
        },
    )));
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental invalid Epic import", false)
        .with_ability_definition(root.clone())
        .id();
    let mut runner = scenario.build();
    let mut state = runner.state().clone();
    let before = serde_json::to_value(&state).unwrap();
    let mut events = Vec::new();
    let error = resolve_ability_chain(
        &mut state,
        &build_resolved_from_def(&root, spell, P0),
        &mut events,
        0,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("foreign instruction-result dependency"));
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    assert!(events.is_empty());
    runner.cast(spell).resolve();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
    assert_eq!(runner.state().next_return_result_process_id, 1);
    departure_assert_program_complete(&runner, spell, 0, 0);
}

fn departure_independent_epic_cost(mode: u8) -> engine::types::ability::AbilityCost {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::game_state::ReturnResultProcessId;
    use engine::types::identifiers::ObjectId;
    let definition = if mode == 0 {
        departure_selected_local_body(false, false)
    } else {
        let mut definition = departure_gain_definition(7, P0);
        let Effect::GainLife { amount, .. } = definition.effect.as_mut() else {
            unreachable!()
        };
        if mode == 1 {
            let QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate { source, .. },
            } = amount
            else {
                unreachable!()
            };
            *source = ZoneChangeRecordSource::ThisTurn;
        } else if mode == 2 {
            *amount = QuantityExpr::Fixed { value: 5 };
        }
        definition
    };
    let mut snapshot = build_resolved_from_def(&definition, ObjectId(0), P0);
    snapshot.return_result_process = Some(ReturnResultProcessId(901));
    AbilityCost::EffectCost {
        effect: Box::new(Effect::EpicCopy {
            spell: Box::new(snapshot),
        }),
    }
}

#[test]
fn departure_payment_ordinary_alternative_and_decline_do_not_import_independent_epic_results() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::actions::{GameAction, UnlessCostBranch};
    use engine::types::game_state::WaitingFor;
    for mode in 0..3 {
        for decline in [false, true] {
            let (mut runner, spell, _) = departure_public_program_runner(false, false, 2, |_| {
                let mut body = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
                body.unless_pay = Some(UnlessPayModifier {
                    cost: AbilityCost::OneOf {
                        costs: vec![
                            AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                            },
                            departure_independent_epic_cost(mode),
                        ],
                    },
                    payer: TargetFilter::Controller,
                });
                let mut tail = departure_dual_reader();
                tail.sub_link = SubAbilityLink::SequentialSibling;
                body.sub_ability = Some(Box::new(tail));
                (body, vec![])
            });
            let WaitingFor::UnlessPaymentChooseCost {
                player,
                costs,
                pending_effect,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("the ordinary payable alternative must expose a real public payment choice");
            };
            assert_eq!(*player, P0);
            assert_eq!(costs.len(), 2);
            let AbilityCost::EffectCost { effect } = &costs[1] else {
                unreachable!()
            };
            let Effect::EpicCopy { spell: retained } = effect.as_ref() else {
                unreachable!()
            };
            assert_eq!(
                retained.return_result_process,
                Some(engine::types::game_state::ReturnResultProcessId(901))
            );
            assert!(pending_effect.return_result_process.is_some());
            let authoritative = serde_json::to_value(runner.state()).unwrap();
            assert!(authoritative.to_string().contains("901"));
            departure_assert_private_result_views(runner.state());
            *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
            runner
                .act(GameAction::ChooseUnlessCostBranch {
                    choice: if decline {
                        UnlessCostBranch::Decline
                    } else {
                        UnlessCostBranch::Pay { index: 0 }
                    },
                })
                .unwrap();
            departure_assert_program_complete(&runner, spell, if decline { 5 } else { 4 }, 2);
            assert!(
                runner.state().stack.is_empty(),
                "the unsupported Epic cost alternative was never executed"
            );
        }
    }
}

#[test]
fn departure_nested_mana_payment_custody_validates_its_independent_epic_alternative_separately() {
    use engine::types::ability::AbilityCost;
    use engine::types::game_state::{ManaAbilityResume, PendingCostMoveResume};
    for effect_cost in [false, true] {
        let (mut runner, spell, sources) =
            departure_payment_mana_child_runner(effect_cost, false, false);
        let mut reached_count = 0;
        departure_finish_payment_mana_child(&mut runner, |reached| {
            reached_count += 1;
            // Supplemental typed custody: Epic is not a payable EffectCost.
            // The public source/payment pause is real; only the retained cost
            // alternative is changed here to isolate the second validator.
            let mut custody = reached.state().clone();
            let Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) =
                &mut custody.pending_cost_move_resume
            else {
                unreachable!()
            };
            let cost = match &mut pending.resume {
                ManaAbilityResume::UnlessPayment { cost, .. }
                | ManaAbilityResume::EffectPayCost { cost, .. } => cost,
                _ => unreachable!(),
            };
            **cost = AbilityCost::OneOf {
                costs: vec![(**cost).clone(), departure_independent_epic_cost(0)],
            };
            let saved = serde_json::to_value(&custody).unwrap();
            let loaded: GameState = serde_json::from_value(saved.clone())
                .expect("local R11 is not a missing parent R7 payment result");
            assert_eq!(serde_json::to_value(loaded).unwrap(), saved);
        });
        assert_eq!(reached_count, 1);
        assert_eq!(runner.state().objects[&sources[0]].zone, Zone::Exile);
        departure_assert_program_complete(&runner, spell, 5, -3);
    }
}

#[test]
fn departure_source_only_bolster_prevention_step_and_player_thresholds_refuse_before_the_root() {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::game_state::WaitingFor;
    use engine::types::phase::Phase;
    for owner in 0..7 {
        for mode in 0..3 {
            for empty in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let mut victims = vec![];
                if !empty {
                    victims.push(scenario.add_creature(P0, "Centaur Courser", 3, 3).id());
                    victims.push(scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
                }
                let survivor = scenario
                    .add_creature(P0, "Supplemental surviving bolster member", 2, 20)
                    .id();
                let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                if mode == 1 {
                    let QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeAggregate { source, .. },
                    } = &mut amount
                    else {
                        unreachable!()
                    };
                    *source = ZoneChangeRecordSource::ThisTurn;
                } else if mode == 2 {
                    amount = QuantityExpr::Fixed { value: 3 };
                }
                let filter = PlayerFilter::ControlsCount {
                    relation: PlayerRelation::Controller,
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                    comparator: Comparator::LT,
                    count: Box::new(amount.clone()),
                };
                let effect = match owner {
                    0 => Effect::Bolster {
                        count: amount.clone(),
                    },
                    1 => Effect::PreventDamage {
                        lifetime: engine::types::ability::RedirectionLifetime::Continuous,
                        amount: PreventionAmount::All,
                        amount_dynamic: Some(amount.clone()),
                        target: TargetFilter::Controller,
                        recipient_scope: EffectScope::All,
                        scope: PreventionScope::AllDamage,
                        damage_source_filter: None,
                        prevention_duration: None,
                    },
                    2 => Effect::SkipNextStep {
                        target: TargetFilter::Controller,
                        step: StepSkipTarget::Step(Phase::Draw),
                        count: amount.clone(),
                        scope: SkipScope::NextOccurrence,
                    },
                    3 => Effect::GainLife {
                        player: TargetFilter::Controller,
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    4 => Effect::StartYourEngines {
                        player_scope: filter.clone(),
                    },
                    5 => Effect::DamageEachPlayer {
                        player_filter: filter.clone(),
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    6 => Effect::DamageAll {
                        target: TargetFilter::None,
                        amount: QuantityExpr::Fixed { value: 1 },
                        player_filter: Some(filter.clone()),
                        damage_source: None,
                    },
                    _ => unreachable!(),
                };
                let mut body = AbilityDefinition::new(AbilityKind::Spell, effect);
                if owner == 3 {
                    body.player_scope = Some(filter);
                }
                let mut root = departure_exile_definition(7);
                let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
                    unreachable!()
                };
                *target = TargetFilter::Or {
                    filters: victims
                        .iter()
                        .map(|id| TargetFilter::SpecificObject { id: *id })
                        .collect(),
                };
                root.sub_ability = Some(Box::new(body));
                let spell = scenario
                    .add_spell_to_hand(P0, "Supplemental source-only consumer", false)
                    .with_ability_definition(root.clone())
                    .id();
                let damage = scenario
                    .add_spell_to_hand(P0, "Supplemental later damage event", false)
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::DealDamage {
                            amount: QuantityExpr::Fixed { value: 8 },
                            target: TargetFilter::Controller,
                            damage_source: None,
                            excess: None,
                        },
                    ))
                    .id();
                for seat in [P0, P1] {
                    for _ in 0..8 {
                        scenario.add_card_to_library_top(seat, "Library witness");
                    }
                }
                let mut runner = scenario.build();
                if mode == 0 {
                    let mut state = runner.state().clone();
                    let before = serde_json::to_value(&state).unwrap();
                    let mut events = vec![];
                    let error = resolve_ability_chain(
                        &mut state,
                        &build_resolved_from_def(&root, spell, P0),
                        &mut events,
                        0,
                    )
                    .unwrap_err();
                    assert!(format!("{error:?}").contains("executable context"));
                    assert_eq!(serde_json::to_value(&state).unwrap(), before);
                    assert!(events.is_empty());
                }
                runner.cast(spell).resolve();
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(runner.state().stack.is_empty());
                assert!(runner.state().resolution_stack.is_empty());
                assert!(runner.state().return_result_frames.is_empty());
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                for victim in &victims {
                    assert_eq!(
                        runner.state().objects[victim].zone,
                        if mode == 0 {
                            Zone::Battlefield
                        } else {
                            Zone::Exile
                        }
                    );
                }
                let value = if mode == 2 {
                    3
                } else if empty {
                    0
                } else {
                    5
                };
                if mode == 0 {
                    assert_eq!(runner.state().next_return_result_process_id, 1);
                    assert_eq!(runner.state().players[0].life, 20);
                    assert!(runner.state().pending_damage_replacements.is_empty());
                } else {
                    match owner {
                        0 => assert_eq!(
                            runner.state().objects[&survivor]
                                .counters
                                .get(&CounterType::Plus1Plus1)
                                .copied()
                                .unwrap_or(0),
                            value
                        ),
                        1 => {
                            runner.cast(damage).resolve();
                            departure_assert_program_complete(
                                &runner,
                                spell,
                                -(8 - value as i32),
                                0,
                            );
                        }
                        2 => assert_eq!(
                            runner.state().steps_to_skip[0]
                                .get(&Phase::Draw)
                                .copied()
                                .unwrap_or(0),
                            value
                        ),
                        3 => assert_eq!(runner.state().players[0].life, 20 + i32::from(value > 1)),
                        4 => assert_eq!(
                            runner.state().players[0].speed,
                            if value > 1 { Some(1) } else { None }
                        ),
                        5 | 6 => {
                            assert_eq!(runner.state().players[0].life, 20 - i32::from(value > 1))
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
}

#[test]
fn departure_direct_damage_shields_validate_later_formulas_and_both_filter_axes_before_installing()
{
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    for slot in 0..5 {
        for mode in 0..4 {
            for empty in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let victims = if empty {
                    vec![]
                } else {
                    vec![
                        scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
                        scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                    ]
                };
                let recipient = scenario
                    .add_creature(P0, "Supplemental damage recipient", 1, 30)
                    .with_mana_cost(ManaCost::generic(2))
                    .id();
                let Effect::GainLife { mut amount, .. } =
                    *departure_gain_definition(if mode == 3 { 99 } else { 7 }, P0).effect
                else {
                    unreachable!()
                };
                if mode == 1 {
                    let QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeAggregate { source, .. },
                    } = &mut amount
                    else {
                        unreachable!()
                    };
                    *source = ZoneChangeRecordSource::ThisTurn;
                } else if mode == 2 {
                    amount = QuantityExpr::Fixed { value: 3 };
                }
                let mut typed = TypedFilter::default();
                typed.properties.push(FilterProp::Cmc {
                    comparator: Comparator::LT,
                    value: amount.clone(),
                });
                let filter = TargetFilter::Typed(typed);
                let effect = if slot < 3 {
                    Effect::CreateDamageReplacement {
                        source_filter: (slot == 2).then_some(filter.clone()),
                        combat_scope: None,
                        target_filter: None,
                        modification: Some(match slot {
                            0 => DamageModification::Plus {
                                value: amount.clone(),
                            },
                            1 => DamageModification::PreventionMinus {
                                value: PreventionFormula::Quantity {
                                    quantity: amount.clone(),
                                },
                            },
                            _ => DamageModification::Double,
                        }),
                        redirect_to: None,
                        redirect_amount: None,
                        redirect_object_filter: None,
                        recipient_object_filter: None,
                        redirect_lifetime: Default::default(),
                    }
                } else {
                    Effect::PreventDamage {
                        lifetime: engine::types::ability::RedirectionLifetime::Continuous,
                        amount: PreventionAmount::All,
                        amount_dynamic: None,
                        target: if slot == 4 {
                            filter.clone()
                        } else {
                            TargetFilter::Controller
                        },
                        recipient_scope: EffectScope::All,
                        scope: PreventionScope::AllDamage,
                        damage_source_filter: (slot == 3).then_some(filter),
                        prevention_duration: None,
                    }
                };
                let mut root = departure_exile_definition(7);
                let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
                    unreachable!()
                };
                *target = TargetFilter::Or {
                    filters: victims
                        .iter()
                        .map(|id| TargetFilter::SpecificObject { id: *id })
                        .collect(),
                };
                root.sub_ability =
                    Some(Box::new(AbilityDefinition::new(AbilityKind::Spell, effect)));
                let spell = scenario
                    .add_spell_to_hand(P0, "Supplemental direct shield constructor", false)
                    .with_ability_definition(root.clone())
                    .id();
                let damage = scenario
                    .add_spell_to_hand(P0, "Supplemental actual damage", false)
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::DealDamage {
                            amount: QuantityExpr::Fixed { value: 8 },
                            target: if slot == 4 {
                                TargetFilter::SpecificObject { id: recipient }
                            } else {
                                TargetFilter::Controller
                            },
                            damage_source: None,
                            excess: None,
                        },
                    ))
                    .with_mana_cost(ManaCost::generic(2))
                    .id();
                scenario.with_mana_pool(
                    P0,
                    vec![ManaUnit::new(ManaType::Black, damage, false, vec![]); 2],
                );
                for player in [P0, P1] {
                    for _ in 0..8 {
                        scenario.add_card_to_library_top(player, "Library witness");
                    }
                }
                let mut runner = scenario.build();
                if mode == 0 || mode == 3 {
                    let mut state = runner.state().clone();
                    let before = serde_json::to_value(&state).unwrap();
                    let mut events = vec![];
                    let error = resolve_ability_chain(
                        &mut state,
                        &build_resolved_from_def(&root, spell, P0),
                        &mut events,
                        0,
                    )
                    .unwrap_err();
                    assert!(format!("{error:?}").contains("instruction-result"));
                    assert_eq!(serde_json::to_value(&state).unwrap(), before);
                    assert!(events.is_empty());
                }
                runner.cast(spell).resolve();
                departure_assert_program_complete(&runner, spell, 0, 0);
                if mode == 0 || mode == 3 {
                    for victim in &victims {
                        assert_eq!(runner.state().objects[victim].zone, Zone::Battlefield);
                    }
                    assert!(runner.state().pending_damage_replacements.is_empty());
                    assert_eq!(runner.state().next_return_result_process_id, 1);
                    continue;
                }
                assert_eq!(
                    runner.state().pending_damage_replacements.len(),
                    1,
                    "ordinary constructor actually installs the shield"
                );
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).unwrap();
                let value = if mode == 2 {
                    3
                } else if empty {
                    0
                } else {
                    5
                };
                if slot == 4 {
                    runner.cast(damage).target_object(recipient).resolve();
                } else {
                    runner.cast(damage).resolve();
                }
                let damage_count = match slot {
                    0 => 8 + value,
                    1 => 8 - value,
                    2 => {
                        if value > 2 {
                            16
                        } else {
                            8
                        }
                    }
                    3 | 4 => {
                        if value > 2 {
                            0
                        } else {
                            8
                        }
                    }
                    _ => unreachable!(),
                };
                if slot == 4 {
                    assert_eq!(
                        runner.state().objects[&recipient].damage_marked,
                        damage_count
                    );
                }
                departure_assert_program_complete(
                    &runner,
                    spell,
                    if slot == 4 { 0 } else { -(damage_count as i32) },
                    0,
                );
                assert_eq!(
                    runner.state().players[0].mana_pool.total(),
                    0,
                    "actual later damage spell paid its whole cost"
                );
                assert_eq!(runner.state().objects[&damage].zone, Zone::Graveyard);
            }
        }
    }
}

#[test]
fn departure_prevention_rider_installs_unbound_and_gets_a_fresh_process_on_actual_later_damage() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for sibling in [false, true] {
        for mode in 0..3 {
            if sibling && mode != 0 {
                continue;
            }
            for empty in [false, true] {
                let (mut runner, spell, subjects) =
                    departure_public_program_runner(false, empty, 2, |scenario| {
                        let damage = scenario
                            .add_spell_to_hand(P0, "Supplemental later rider damage", false)
                            .with_ability_definition(AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::DealDamage {
                                    amount: QuantityExpr::Fixed { value: 1 },
                                    target: TargetFilter::Controller,
                                    damage_source: None,
                                    excess: None,
                                },
                            ))
                            .id();
                        let mut shield = AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::PreventDamage {
                                lifetime: engine::types::ability::RedirectionLifetime::Continuous,
                                amount: PreventionAmount::All,
                                amount_dynamic: None,
                                target: TargetFilter::Controller,
                                recipient_scope: EffectScope::All,
                                scope: PreventionScope::AllDamage,
                                damage_source_filter: None,
                                prevention_duration: None,
                            },
                        );
                        let mut rider = if sibling {
                            departure_gain_definition(7, P0)
                        } else if mode == 0 {
                            departure_selected_local_body(true, false)
                        } else {
                            let mut ordinary = departure_gain_definition(7, P0);
                            let Effect::GainLife { amount, .. } = ordinary.effect.as_mut() else {
                                unreachable!()
                            };
                            if mode == 1 {
                                let QuantityExpr::Ref {
                                    qty: QuantityRef::ZoneChangeAggregate { source, .. },
                                } = amount
                                else {
                                    unreachable!()
                                };
                                *source = ZoneChangeRecordSource::ThisTurn;
                            } else {
                                *amount = QuantityExpr::Fixed { value: 4 };
                            }
                            ordinary
                        };
                        rider.sub_link = if sibling {
                            SubAbilityLink::SequentialSibling
                        } else {
                            SubAbilityLink::ContinuationStep
                        };
                        shield.sub_ability = Some(Box::new(rider));
                        (shield, vec![damage])
                    });
                let before_damage_process = runner.state().next_return_result_process_id;
                let shield = runner.state().pending_damage_replacements.last().unwrap();
                assert_eq!(shield.runtime_execute.is_some(), !sibling);
                if let Some(rider) = &shield.runtime_execute {
                    assert!(rider.return_result_process.is_none());
                }
                departure_assert_program_complete(
                    &runner,
                    spell,
                    if sibling && !empty { 5 } else { 0 },
                    0,
                );
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).unwrap();
                runner.cast(subjects[0]).resolve();
                if !sibling && mode == 0 {
                    let WaitingFor::ChooseOneOfBranch { player, .. } = &runner.state().waiting_for
                    else {
                        panic!("the actual prevention event invokes the complete local rider and reaches its public child");
                    };
                    assert_eq!(*player, P0);
                    assert!(runner.state().next_return_result_process_id > before_damage_process);
                    let saved = serde_json::to_value(runner.state()).unwrap();
                    *runner.state_mut() = serde_json::from_value(saved).unwrap();
                    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
                }
                departure_assert_program_complete(
                    &runner,
                    spell,
                    if sibling || mode == 1 {
                        if empty {
                            0
                        } else {
                            5
                        }
                    } else if mode == 2 {
                        4
                    } else {
                        3
                    },
                    0,
                );
                assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Graveyard);
            }
        }
    }
}

#[test]
fn departure_unselected_epic_cost_and_installed_rider_cannot_import_the_callers_result() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::phase::Phase;
    for cost_payload in [false, true] {
        for empty in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let victim = (!empty).then(|| scenario.add_creature(P0, "Centaur Courser", 3, 3).id());
            let mut body = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
            if cost_payload {
                body.unless_pay = Some(UnlessPayModifier {
                    cost: AbilityCost::OneOf {
                        costs: vec![
                            AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                            },
                            departure_independent_epic_cost(3),
                        ],
                    },
                    payer: TargetFilter::Controller,
                });
            } else {
                body.effect = Box::new(Effect::PreventDamage {
                    lifetime: engine::types::ability::RedirectionLifetime::Continuous,
                    amount: PreventionAmount::All,
                    amount_dynamic: None,
                    target: TargetFilter::Controller,
                    recipient_scope: EffectScope::All,
                    scope: PreventionScope::AllDamage,
                    damage_source_filter: None,
                    prevention_duration: None,
                });
                let mut imported = departure_gain_definition(7, P0);
                imported.sub_link = SubAbilityLink::ContinuationStep;
                body.sub_ability = Some(Box::new(imported));
            }
            let mut root = departure_exile_definition(7);
            root.sub_ability = Some(Box::new(body));
            let spell = scenario
                .add_spell_to_hand(
                    P0,
                    "Supplemental independent payload refuses caller import",
                    false,
                )
                .with_ability_definition(root.clone())
                .id();
            let mut runner = scenario.build();
            let mut state = runner.state().clone();
            let before = serde_json::to_value(&state).unwrap();
            let mut events = vec![];
            let error = resolve_ability_chain(
                &mut state,
                &build_resolved_from_def(&root, spell, P0),
                &mut events,
                0,
            )
            .unwrap_err();
            assert!(format!("{error:?}").contains("foreign instruction-result dependency"));
            assert_eq!(serde_json::to_value(&state).unwrap(), before);
            assert!(events.is_empty());
            runner.cast(spell).resolve();
            if let Some(victim) = victim {
                assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
            }
            assert!(runner.state().pending_damage_replacements.is_empty());
            assert_eq!(runner.state().next_return_result_process_id, 1);
            departure_assert_program_complete(&runner, spell, 0, 0);
        }
    }
}

#[test]
fn departure_direct_shield_source_choice_latches_identity_for_actual_later_damage() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::phase::Phase;
    for prevention in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
        let damage = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
        );
        let chosen = scenario
            .add_creature(P0, "Supplemental chosen damage source", 1, 9)
            .with_ability_definition(damage.clone())
            .id();
        let other = scenario
            .add_creature(P0, "Supplemental other damage source", 1, 9)
            .with_ability_definition(damage)
            .id();
        let filter = Some(TargetFilter::ChosenDamageSource {
            chooser: Some(engine::types::ability::Chooser::Controller),
            filter: None,
        });
        let effect = if prevention {
            Effect::PreventDamage {
                lifetime: engine::types::ability::RedirectionLifetime::Continuous,
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Controller,
                recipient_scope: EffectScope::All,
                scope: PreventionScope::AllDamage,
                damage_source_filter: filter,
                prevention_duration: None,
            }
        } else {
            Effect::CreateDamageReplacement {
                source_filter: filter,
                combat_scope: None,
                target_filter: None,
                modification: Some(DamageModification::PreventionMinus {
                    value: PreventionFormula::Fixed(99),
                }),
                redirect_to: None,
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
                redirect_lifetime: Default::default(),
            }
        };
        let mut root = departure_exile_definition(7);
        let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
            unreachable!()
        };
        *target = TargetFilter::SpecificObject { id: victim };
        root.sub_ability = Some(Box::new(AbilityDefinition::new(AbilityKind::Spell, effect)));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental shield source identity", false)
            .with_ability_definition(root)
            .id();
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("ordinary chosen-source constructor reaches its actual public choice");
        };
        assert!(options
            .iter()
            .any(|option| option.identity.reference.object_id == chosen));
        assert!(options
            .iter()
            .any(|option| option.identity.reference.object_id == other));
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        runner
            .act(GameAction::ChooseDamageSource {
                source: engine::types::identifiers::ObjectIncarnationRef::of(
                    chosen,
                    runner.state().objects[&chosen].incarnation,
                ),
            })
            .unwrap();
        departure_assert_program_complete(&runner, spell, 0, 0);
        assert_eq!(
            runner
                .state()
                .pending_damage_replacements
                .last()
                .unwrap()
                .damage_source_filter,
            Some(TargetFilter::SpecificObject { id: chosen })
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        runner.activate(chosen, 0).resolve();
        departure_assert_program_complete(&runner, spell, 0, 0);
        runner.activate(other, 0).resolve();
        departure_assert_program_complete(&runner, spell, -3, 0);
    }
}

#[test]
fn departure_source_only_object_predicate_refuses_before_root_and_ordinary_filter_reaches_public_tail(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;
    for mode in 0..3 {
        for empty in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let victims = if empty {
                vec![]
            } else {
                vec![
                    scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
                    scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                ]
            };
            let member = scenario
                .add_creature(P0, "Supplemental remembered filter member", 2, 20)
                .with_mana_cost(ManaCost::generic(2))
                .id();
            let Effect::GainLife { mut amount, .. } = *departure_gain_definition(7, P0).effect
            else {
                unreachable!()
            };
            if mode == 1 {
                let QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeAggregate { source, .. },
                } = &mut amount
                else {
                    unreachable!()
                };
                *source = ZoneChangeRecordSource::ThisTurn;
            } else if mode == 2 {
                amount = QuantityExpr::Fixed { value: 3 };
            }
            let mut filter = TypedFilter::creature();
            filter.properties.extend([
                FilterProp::InZone {
                    zone: Zone::Battlefield,
                },
                FilterProp::Cmc {
                    comparator: Comparator::LT,
                    value: amount,
                },
            ]);
            let mut body = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RememberCard {
                    target: TargetFilter::Typed(filter),
                },
            );
            body.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)],
                    kind: ChooseOneOfKind::Ordinary,
                },
            )));
            let mut root = departure_exile_definition(7);
            let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
                unreachable!()
            };
            *target = TargetFilter::Or {
                filters: victims
                    .iter()
                    .map(|id| TargetFilter::SpecificObject { id: *id })
                    .collect(),
            };
            root.sub_ability = Some(Box::new(body));
            let spell = scenario
                .add_spell_to_hand(P0, "Supplemental source-only object predicate", false)
                .with_ability_definition(root.clone())
                .id();
            let mut runner = scenario.build();
            if mode == 0 {
                let mut state = runner.state().clone();
                let before = serde_json::to_value(&state).unwrap();
                let mut events = vec![];
                let error = resolve_ability_chain(
                    &mut state,
                    &build_resolved_from_def(&root, spell, P0),
                    &mut events,
                    0,
                )
                .unwrap_err();
                assert!(
                    format!("{error:?}").contains("object predicate requires executable context")
                );
                assert_eq!(serde_json::to_value(&state).unwrap(), before);
                assert!(events.is_empty());
            }
            runner.cast(spell).resolve();
            if mode == 0 {
                for victim in &victims {
                    assert_eq!(runner.state().objects[victim].zone, Zone::Battlefield);
                }
                assert_eq!(runner.state().next_return_result_process_id, 1);
            } else {
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ChooseOneOfBranch { .. }
                ));
                let selected = runner.state().objects[&spell].chosen_attributes.iter().any(|choice| matches!(choice, ChosenAttribute::Card(pin) if pin.object_id == member));
                assert_eq!(selected, mode == 2 || !empty, "the actual source-only filter reads the fixed/turn quantity while the source is still on stack");
                let saved = serde_json::to_value(runner.state()).unwrap();
                *runner.state_mut() = serde_json::from_value(saved).unwrap();
                runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
            }
            departure_assert_program_complete(&runner, spell, 0, 0);
        }
    }
}

#[test]
fn departure_five_contextual_source_only_thresholds_refuse_before_the_complete_root() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::game_state::WaitingFor;
    for owner in 0..9 {
        for recursive in [false, true] {
            for mode in 0..3 {
                for empty in [false, true] {
                    let mut scenario = GameScenario::new();
                    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
                    let retained = scenario
                        .add_artifact_from_oracle(P0, "Supplemental player predicate witness", "")
                        .id();
                    let victims = if empty {
                        vec![]
                    } else {
                        vec![
                            scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
                            scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                        ]
                    };
                    let Effect::GainLife { mut amount, .. } =
                        *departure_gain_definition(7, P0).effect
                    else {
                        unreachable!()
                    };
                    if mode == 1 {
                        let QuantityExpr::Ref {
                            qty: QuantityRef::ZoneChangeAggregate { source, .. },
                        } = &mut amount
                        else {
                            unreachable!()
                        };
                        *source = ZoneChangeRecordSource::ThisTurn;
                    } else if mode == 2 {
                        amount = QuantityExpr::Fixed { value: 3 };
                    }
                    if recursive {
                        amount = QuantityExpr::Sum {
                            exprs: vec![
                                QuantityExpr::Fixed { value: 0 },
                                QuantityExpr::Offset {
                                    inner: Box::new(amount),
                                    offset: 0,
                                },
                            ],
                        };
                    }
                    let mut filter = PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Controller,
                        attr: Box::new(QuantityRef::LifeTotal {
                            player: PlayerScope::ScopedPlayer,
                        }),
                        comparator: Comparator::GE,
                        value: Box::new(amount.clone()),
                    };
                    if recursive {
                        filter = PlayerFilter::AnyOf {
                            filters: vec![PlayerFilter::AllExcept {
                                exclude: Box::new(PlayerFilter::AllExcept {
                                    exclude: Box::new(filter),
                                }),
                            }],
                        };
                    }
                    let effect = match owner {
                        0 | 4 => Effect::GainLife {
                            player: TargetFilter::Controller,
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        1 => Effect::StartYourEngines {
                            player_scope: filter.clone(),
                        },
                        2 => Effect::DamageEachPlayer {
                            player_filter: filter.clone(),
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        3 => Effect::DamageAll {
                            target: TargetFilter::None,
                            amount: QuantityExpr::Fixed { value: 1 },
                            player_filter: Some(filter.clone()),
                            damage_source: None,
                        },
                        5 => Effect::GainLife {
                            player: TargetFilter::Controller,
                            amount: QuantityExpr::Ref {
                                qty: QuantityRef::EventContextPlayerCount {
                                    filter: filter.clone(),
                                },
                            },
                        },
                        6 | 7 => Effect::GainLife {
                            player: TargetFilter::Controller,
                            amount: QuantityExpr::Ref {
                                qty: QuantityRef::PlayerCount {
                                    filter: if owner == 6 {
                                        PlayerFilter::AnyOf {
                                            filters: vec![filter.clone()],
                                        }
                                    } else {
                                        PlayerFilter::AllExcept {
                                            exclude: Box::new(PlayerFilter::AllExcept {
                                                exclude: Box::new(filter.clone()),
                                            }),
                                        }
                                    },
                                },
                            },
                        },
                        8 => Effect::GainLife {
                            player: TargetFilter::Controller,
                            amount: QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount {
                                    filter: TargetFilter::And {
                                        filters: vec![
                                            TargetFilter::SpecificObject { id: retained },
                                            TargetFilter::Typed({
                                                let mut typed = TypedFilter::default();
                                                typed.properties.push(
                                                    FilterProp::ControllerMatches {
                                                        player: Box::new(filter.clone()),
                                                    },
                                                );
                                                typed
                                            }),
                                        ],
                                    },
                                },
                            },
                        },
                        _ => unreachable!(),
                    };
                    let mut body = AbilityDefinition::new(AbilityKind::Spell, effect);
                    if owner == 0 {
                        body.player_scope = Some(filter);
                    }
                    if owner == 4 {
                        let mut condition = AbilityCondition::PreviousEffectAmount {
                            comparator: Comparator::GT,
                            rhs: QuantityExpr::Offset {
                                inner: Box::new(amount),
                                offset: -100,
                            },
                            channel: DamageChannel::Total,
                        };
                        if recursive {
                            condition = AbilityCondition::And {
                                conditions: vec![AbilityCondition::Or {
                                    conditions: vec![AbilityCondition::Not {
                                        condition: Box::new(AbilityCondition::Not {
                                            condition: Box::new(condition),
                                        }),
                                    }],
                                }],
                            };
                        }
                        body.condition = Some(condition);
                    }
                    let mut root = departure_exile_definition(7);
                    let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
                        unreachable!()
                    };
                    *target = TargetFilter::Or {
                        filters: victims
                            .iter()
                            .map(|id| TargetFilter::SpecificObject { id: *id })
                            .collect(),
                    };
                    root.sub_ability = Some(Box::new(body));
                    let spell = scenario
                        .add_spell_to_hand(P0, "Supplemental contextual threshold", false)
                        .with_ability_definition(root.clone())
                        .id();
                    let mut runner = scenario.build();
                    if mode == 0 {
                        let mut state = runner.state().clone();
                        let before = serde_json::to_value(&state).unwrap();
                        let mut events = vec![];
                        let error = resolve_ability_chain(
                            &mut state,
                            &build_resolved_from_def(&root, spell, P0),
                            &mut events,
                            0,
                        )
                        .unwrap_err();
                        assert!(
                            format!("{error:?}").contains("executable context"),
                            "owner {owner}, recursive {recursive}: {error:?}"
                        );
                        assert_eq!(serde_json::to_value(&state).unwrap(), before);
                        assert!(events.is_empty());
                    }
                    runner.cast(spell).resolve();
                    for victim in &victims {
                        assert_eq!(
                            runner.state().objects[victim].zone,
                            if mode == 0 {
                                Zone::Battlefield
                            } else {
                                Zone::Exile
                            }
                        );
                    }
                    if mode == 0 {
                        assert_eq!(runner.state().next_return_result_process_id, 1);
                    } else if owner == 1 {
                        assert_eq!(runner.state().players[0].speed, Some(1));
                    }
                    // No batched damage trigger exists in this spell fixture; EventContextPlayerCount's ordinary no-event value remains zero.
                    let gain = if mode == 0 || owner == 1 || owner == 5 {
                        0
                    } else if owner == 2 || owner == 3 {
                        -1
                    } else {
                        1
                    };
                    departure_assert_program_complete(&runner, spell, gain, 0);
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                }
            }
        }
    }
}

#[test]
fn departure_bound_player_attribute_filters_and_player_counts_still_read_the_actual_process() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::mana::ManaCost;
    for empty in [false, true] {
        for count in [false, true] {
            let (runner, spell, subjects) =
                departure_public_program_runner(false, empty, 2, |scenario| {
                    let retained = scenario
                        .add_artifact_from_oracle(P0, "Supplemental bound player threshold", "")
                        .with_mana_cost(ManaCost::generic(1))
                        .id();
                    let Effect::GainLife { amount, .. } = *departure_gain_definition(7, P0).effect
                    else {
                        unreachable!()
                    };
                    let filter = PlayerFilter::PlayerAttribute {
                        relation: PlayerRelation::Controller,
                        attr: Box::new(QuantityRef::LifeTotal {
                            player: PlayerScope::ScopedPlayer,
                        }),
                        comparator: Comparator::EQ,
                        value: Box::new(QuantityExpr::Sum {
                            exprs: vec![QuantityExpr::Fixed { value: 20 }, amount],
                        }),
                    };
                    let quantity = QuantityExpr::Ref {
                        qty: QuantityRef::PlayerCount { filter },
                    };
                    let body = Effect::GainLife {
                        player: TargetFilter::Controller,
                        amount: if count {
                            quantity
                        } else {
                            QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount {
                                    filter: TargetFilter::And {
                                        filters: vec![
                                            TargetFilter::SpecificObject { id: retained },
                                            TargetFilter::Typed({
                                                let mut typed = TypedFilter::default();
                                                typed.properties.push(FilterProp::Cmc {
                                                    comparator: Comparator::EQ,
                                                    value: quantity,
                                                });
                                                typed
                                            }),
                                        ],
                                    },
                                },
                            }
                        },
                    };
                    (
                        AbilityDefinition::new(AbilityKind::Spell, body),
                        vec![retained],
                    )
                });
            departure_assert_program_complete(&runner, spell, if empty { 1 } else { 0 }, 0);
            assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn departure_raw_runtime_replacement_rejects_caller_import_before_install_and_runs_its_own_later_process(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ReturnResultProcessId, WaitingFor};
    use engine::types::replacements::ReplacementEvent;
    for imported in [false, true] {
        for empty in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
            let victims = if empty {
                vec![]
            } else {
                vec![
                    scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
                    scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                ]
            };
            let runtime_source = scenario
                .add_artifact_from_oracle(P0, "Supplemental independent runtime source", "")
                .id();
            let body = if imported {
                departure_gain_definition(7, P0)
            } else {
                departure_selected_local_body(true, false)
            };
            let mut runtime = build_resolved_from_def(&body, runtime_source, P0);
            runtime.return_result_process = Some(ReturnResultProcessId(901));
            let mut replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone);
            replacement.shield_kind = ShieldKind::Prevention {
                amount: PreventionAmount::All,
            };
            replacement.runtime_execute = Some(Box::new(runtime));
            let mut root = departure_exile_definition(7);
            let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
                unreachable!()
            };
            *target = TargetFilter::Or {
                filters: victims
                    .iter()
                    .map(|id| TargetFilter::SpecificObject { id: *id })
                    .collect(),
            };
            root.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AddTargetReplacement {
                    replacement: Box::new(replacement),
                    target: TargetFilter::None,
                },
            )));
            let spell = scenario
                .add_spell_to_hand(P0, "Supplemental raw runtime installer", false)
                .with_ability_definition(root.clone())
                .id();
            let damage = scenario
                .add_spell_to_hand(P0, "Supplemental matching later damage", false)
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        damage_source: None,
                        excess: None,
                    },
                ))
                .id();
            let mut runner = scenario.build();
            if imported {
                let mut state = runner.state().clone();
                let before = serde_json::to_value(&state).unwrap();
                let mut events = vec![];
                let error = resolve_ability_chain(
                    &mut state,
                    &build_resolved_from_def(&root, spell, P0),
                    &mut events,
                    0,
                )
                .unwrap_err();
                assert!(format!("{error:?}").contains("foreign instruction-result dependency"));
                assert_eq!(serde_json::to_value(&state).unwrap(), before);
                assert!(events.is_empty());
            }
            runner.cast(spell).resolve();
            departure_assert_program_complete(&runner, spell, 0, 0);
            for victim in &victims {
                assert_eq!(
                    runner.state().objects[victim].zone,
                    if imported {
                        Zone::Battlefield
                    } else {
                        Zone::Exile
                    }
                );
            }
            if imported {
                assert!(runner.state().pending_damage_replacements.is_empty());
                assert_eq!(runner.state().next_return_result_process_id, 1);
                continue;
            }
            let installed = runner.state().pending_damage_replacements.last().unwrap();
            assert!(
                installed.runtime_execute.is_some(),
                "actual AddTargetReplacement installation retained the complete independent body"
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).unwrap();
            let next_process = runner.state().next_return_result_process_id;
            runner.cast(damage).resolve();
            let WaitingFor::ChooseOneOfBranch {
                player,
                branches,
                expected_return_result_process,
                return_result_occurrence,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("an actual matching damage event executes the installed runtime body");
            };
            assert_eq!(*player, P0);
            assert_eq!(branches.len(), 2);
            let process = expected_return_result_process.unwrap();
            assert_ne!(process, ReturnResultProcessId(901));
            assert!(process.0 >= next_process);
            assert_eq!(
                runner.state().return_result_frames[&return_result_occurrence.unwrap()].process,
                Some(process)
            );
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_value(saved).unwrap();
            departure_assert_private_result_views(runner.state());
            runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
            departure_assert_program_complete(&runner, spell, 3, 0);
            assert_eq!(runner.state().objects[&damage].zone, Zone::Graveyard);
        }
    }
}

fn departure_retained_epic_definition(mode: u8) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::*;
    let AbilityCost::EffectCost { effect } = departure_independent_epic_cost(mode) else {
        unreachable!()
    };
    AbilityDefinition::new(AbilityKind::Activated, *effect)
}

fn departure_retained_epic_values(mode: u8) -> engine::types::ability::CopiableValues {
    use engine::types::ability::*;
    use std::sync::Arc;
    CopiableValues {
        name: "Supplemental public copied template".into(),
        mana_cost: Default::default(),
        color: vec![],
        card_types: Default::default(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        keywords: vec![],
        abilities: Arc::new(vec![departure_retained_epic_definition(mode)]),
        trigger_definitions: Arc::new(vec![]),
        trigger_printed_origins: Arc::new(vec![]),
        replacement_definitions: Arc::new(vec![]),
        static_definitions: Arc::new(vec![]),
        room_halves: None,
        prepare_face: None,
        name_origin: Default::default(),
    }
}

#[test]
fn departure_actual_continuous_install_redacts_all_five_retained_definition_arms_before_copying() {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::triggers::TriggerMode;
    for arm in 0..7 {
        for mode in [0, 2] {
            let (mut runner, spell, subjects) =
                departure_public_program_runner(false, false, 2, |scenario| {
                    let recipient = scenario
                        .add_artifact_from_oracle(P0, "Supplemental continuous recipient", "")
                        .id();
                    let definition = departure_retained_epic_definition(mode);
                    let modification = match arm {
                        0 => ContinuousModification::GrantAbility {
                            definition: Box::new(definition),
                        },
                        1 => ContinuousModification::GrantTrigger {
                            trigger: Box::new(
                                TriggerDefinition::new(TriggerMode::SpellCast).execute(definition),
                            ),
                        },
                        2 => {
                            let mut replacement =
                                ReplacementDefinition::new(ReplacementEvent::DamageDone);
                            replacement.runtime_execute = Some(Box::new(build_resolved_from_def(
                                &definition,
                                recipient,
                                P0,
                            )));
                            ContinuousModification::GrantReplacement {
                                replacement: Box::new(replacement),
                            }
                        }
                        3 => ContinuousModification::GrantStaticAbility {
                            definition: Box::new(
                                StaticDefinition::new(StaticMode::Continuous)
                                    .affected(TargetFilter::SelfRef)
                                    .modifications(vec![ContinuousModification::GrantAbility {
                                        definition: Box::new(definition),
                                    }]),
                            ),
                        },
                        4 => ContinuousModification::CopyValues {
                            values: Box::new(departure_retained_epic_values(mode)),
                            display: engine::game::game_object::DisplayIdentity {
                                display_source: Default::default(),
                                printed_ref: None,
                                token_image_ref: None,
                                token_art: None,
                            },
                        },
                        5 => ContinuousModification::RemoveKeyword {
                            keyword: engine::types::keywords::Keyword::CumulativeUpkeep(
                                departure_independent_epic_cost(mode),
                            ),
                        },
                        6 => ContinuousModification::AddStaticMode {
                            mode: StaticMode::CastWithAlternativeCost {
                                cost: departure_independent_epic_cost(mode),
                                timing_permission: None,
                                frequency: Default::default(),
                            },
                        },
                        _ => unreachable!(),
                    };
                    let body = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GenericEffect {
                            static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                                .affected(TargetFilter::SpecificObject { id: recipient })
                                .modifications(vec![modification])],
                            duration: Some(Duration::UntilEndOfTurn),
                            target: None,
                            end_cost: None,
                        },
                    );
                    (body, vec![recipient])
                });
            departure_assert_program_complete(&runner, spell, 0, 0);
            assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().transient_continuous_effects.len(),
                1,
                "actual GenericEffect installed its retained payload"
            );
            let authoritative = serde_json::to_value(runner.state()).unwrap();
            let retained = &authoritative["transient_continuous_effects"];
            assert!(retained
                .to_string()
                .contains("\"return_result_process\":901"));
            *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
            departure_assert_private_result_views(runner.state());
            for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
                let projected = match viewer {
                    Some(seat) => {
                        engine::game::visibility::filter_state_for_viewer(runner.state(), seat)
                    }
                    None => {
                        engine::game::visibility::filter_state_for_unseated_viewer(runner.state())
                    }
                };
                let plain = serde_json::to_value(&projected).unwrap();
                let wire =
                    serde_json::to_value(ClientGameStateRef::wrap(runner.state(), viewer)).unwrap();
                for public in [&plain, &wire["state"]] {
                    let tce = &public["transient_continuous_effects"];
                    assert_eq!(tce.as_array().unwrap().len(), 1);
                    for field in [
                        "id",
                        "source_id",
                        "controller",
                        "timestamp",
                        "duration",
                        "affected",
                        "affected_recipient",
                    ] {
                        assert_eq!(
                            tce[0][field], retained[0][field],
                            "public continuous-effect spine: {field}"
                        );
                    }
                    assert_eq!(
                        tce[0]["modifications"][0]["type"],
                        retained[0]["modifications"][0]["type"]
                    );
                    assert_eq!(tce.to_string().matches("EpicCopy").count(), retained.to_string().matches("EpicCopy").count(), "redaction preserves the executable template rather than dropping the public rider");
                }
            }
            assert_eq!(
                serde_json::to_value(runner.state()).unwrap(),
                authoritative,
                "all projections leave authoritative P901 intact before any copy execution"
            );
        }
    }
}

fn departure_runtime_replacement_epic_cost() -> engine::types::ability::AbilityCost {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::identifiers::ObjectId;
    use engine::types::replacements::ReplacementEvent;
    let definition = departure_retained_epic_definition(0);
    let mut replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone);
    replacement.runtime_execute = Some(Box::new(build_resolved_from_def(
        &definition,
        ObjectId(0),
        P0,
    )));
    AbilityCost::EffectCost {
        effect: Box::new(Effect::AddTargetReplacement {
            replacement: Box::new(replacement),
            target: TargetFilter::None,
        }),
    }
}

#[test]
fn departure_public_cost_keyword_face_and_copiable_supplements_redact_nested_runtime_epic() {
    use engine::game::game_object::CleaveFormState;
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::keywords::*;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::triggers::TriggerMode;
    use std::sync::Arc;
    for additional in 0..4 {
        let (mut runner, spell, subjects) =
            departure_public_program_runner(false, false, 2, |scenario| {
                (
                    AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                    vec![scenario
                        .add_artifact_from_oracle(P0, "Supplemental retained public object", "")
                        .id()],
                )
            });
        departure_assert_program_complete(&runner, spell, 0, 0);
        let cost = departure_runtime_replacement_epic_cost();
        let keywords = vec![
            Keyword::CumulativeUpkeep(cost.clone()),
            Keyword::Escalate(cost.clone()),
            Keyword::Bestow(BestowCost::NonMana(cost.clone())),
            Keyword::Embalm(EmbalmCost::NonMana(cost.clone())),
            Keyword::Eternalize(EternalizeCost::NonMana(cost.clone())),
            Keyword::Cycling(CyclingCost::NonMana(cost.clone())),
            Keyword::Flashback(FlashbackCost::NonMana(cost.clone())),
            Keyword::Escape(EscapeCost::NonMana(cost.clone())),
            Keyword::Evoke(EvokeCost::NonMana(cost.clone())),
            Keyword::Blitz(BlitzCost::NonMana(cost.clone())),
            Keyword::Buyback(BuybackCost::NonMana(cost.clone())),
            Keyword::Echo(EchoCost::NonMana(cost.clone())),
        ];
        let additional_cost = match additional {
            0 => AdditionalCost::Optional {
                cost: cost.clone(),
                repeatability: AdditionalCostRepeatability::Once,
            },
            1 => AdditionalCost::Kicker {
                costs: vec![cost.clone(), cost.clone()],
                repeatability: AdditionalCostRepeatability::Once,
            },
            2 => AdditionalCost::Choice(cost.clone(), cost.clone()),
            3 => AdditionalCost::Required(cost.clone()),
            _ => unreachable!(),
        };
        let definition = departure_retained_epic_definition(0);
        let trigger = TriggerDefinition::new(TriggerMode::SpellCast).execute(definition.clone());
        let static_definition = StaticDefinition::new(StaticMode::Continuous).modifications(vec![
            ContinuousModification::GrantAbility {
                definition: Box::new(definition.clone()),
            },
        ]);
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone);
        replacement.runtime_execute = Some(Box::new(build_resolved_from_def(
            &definition,
            subjects[0],
            P0,
        )));
        // Supplemental typed retained faces/costs on a real public object after
        // a paid producer's retirement; no natural face-switch/payment claim.
        let object = runner.state_mut().objects.get_mut(&subjects[0]).unwrap();
        object.keywords = keywords.clone();
        object.base_keywords = keywords.clone();
        object.cast_spell_keywords = keywords.clone();
        object.additional_cost = Some(additional_cost.clone());
        object.casting_options = vec![SpellCastingOption::alternative_cost(cost.clone())];
        object.abilities = Arc::new(vec![definition.clone()]);
        object.base_abilities = object.abilities.clone();
        object.push_printed_trigger(trigger.clone());
        object.static_definitions.push(static_definition.clone());
        object.base_static_definitions = Arc::new(vec![static_definition.clone()]);
        object.replacement_definitions.push(replacement.clone());
        object.base_replacement_definitions = Arc::new(vec![replacement.clone()]);
        object
            .perpetual_mods
            .push(PerpetualModification::GrantAbility {
                modifications: vec![PerpetualGrantModification::AddKeyword {
                    keyword: keywords[0].clone(),
                }],
            });
        let mut face = engine::game::printed_cards::snapshot_object_face(object);
        face.name = "Supplemental public stored face".into();
        face.keywords = keywords.clone();
        face.additional_cost = Some(additional_cost);
        face.casting_options = vec![SpellCastingOption::alternative_cost(cost)];
        object.back_face = Some(face.clone());
        object.specialize_faces = Some(
            [(engine::types::mana::ManaColor::Green, face.clone())]
                .into_iter()
                .collect(),
        );
        let mut prepare_face = face.clone();
        prepare_face.layout_kind = Some(engine::types::card::LayoutKind::Prepare);
        object.copied_prepare_face = Some(Arc::new(prepare_face.clone()));
        object.cleave_form = Some(CleaveFormState {
            abilities: object.abilities.clone(),
            triggers: object.trigger_definitions.clone(),
            statics: object.static_definitions.clone(),
            replacements: object.replacement_definitions.clone(),
            base_abilities: object.base_abilities.clone(),
            base_triggers: object.base_trigger_definitions.clone(),
            base_trigger_printed_origins: object.base_trigger_printed_origins.clone(),
            trigger_base_set_instance: object.trigger_base_set_instance,
            next_trigger_base_set_instance: object.next_trigger_base_set_instance,
            base_statics: object.base_static_definitions.clone(),
            base_replacements: object.base_replacement_definitions.clone(),
        });
        let mut values = departure_retained_epic_values(0);
        values.prepare_face = Some(Arc::new(prepare_face));
        values.keywords = keywords;
        runner
            .state_mut()
            .lki_copiable_values
            .insert(subjects[0], values.clone());
        let incarnation = runner.state().objects[&subjects[0]].incarnation;
        runner
            .state_mut()
            .lki_copiable_values_by_incarnation
            .entry(subjects[0])
            .or_default()
            .insert(incarnation, values);
        let authoritative = serde_json::to_value(runner.state()).unwrap();
        assert!(authoritative
            .to_string()
            .contains("\"return_result_process\":901"));
        *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
        departure_assert_private_result_views(runner.state());
        for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
            let projected = match viewer {
                Some(seat) => {
                    engine::game::visibility::filter_state_for_viewer(runner.state(), seat)
                }
                None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
            };
            let public = &projected.objects[&subjects[0]];
            assert_eq!(public.name, runner.state().objects[&subjects[0]].name);
            assert_eq!(public.keywords.len(), 12);
            assert_eq!(public.base_keywords.len(), 12);
            assert_eq!(public.cast_spell_keywords.len(), 12);
            assert!(public.additional_cost.is_some());
            assert_eq!(public.casting_options.len(), 1);
            assert_eq!(
                public.back_face.as_ref().unwrap().name,
                "Supplemental public stored face"
            );
            assert_eq!(public.specialize_faces.as_ref().unwrap().len(), 1);
            assert!(public.cleave_form.is_some());
            assert!(public.copied_prepare_face.is_some());
            assert!(projected.lki_copiable_values[&subjects[0]]
                .prepare_face
                .is_some());
        }
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
    }
}

#[test]
fn departure_current_and_stored_free_cast_riders_keep_public_payload_without_epic_authority() {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::game_state::{CastOfferKind, WaitingFor};
    let (mut runner, spell, _) = departure_public_program_runner(false, false, 2, |_| {
        (
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
            vec![],
        )
    });
    departure_assert_program_complete(&runner, spell, 0, 0);
    // Supplemental typed public permission custody; this does not claim that
    // a new Oracle source naturally emitted this rider.
    runner.state_mut().waiting_for = WaitingFor::CastOffer {
        player: P0,
        kind: CastOfferKind::FreeCastWindow {
            candidates: vec![spell],
            remaining_casts: Some(2),
            remaining_mv_budget: Some(6),
            face_policy: ResolutionCastFacePolicy::new(TargetFilter::Any, spell, P0, None),
            zones: vec![Zone::Graveyard],
            graveyard_replacement: None,
            member_pool: vec![spell],
            enters_with_modifications: PendingEntersModifications {
                modifications: vec![ContinuousModification::GrantAbility {
                    definition: Box::new(departure_retained_epic_definition(0)),
                }],
                cast_filter: None,
            },
        },
    };
    let authoritative = serde_json::to_value(runner.state()).unwrap();
    assert!(authoritative
        .to_string()
        .contains("\"return_result_process\":901"));
    *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
    departure_assert_private_result_views(runner.state());
    for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
        let wire = serde_json::to_value(ClientGameStateRef::wrap(runner.state(), viewer)).unwrap();
        let WaitingFor::CastOffer {
            player,
            kind:
                CastOfferKind::FreeCastWindow {
                    enters_with_modifications,
                    remaining_casts,
                    remaining_mv_budget,
                    ..
                },
        } = match viewer {
            Some(seat) => engine::game::visibility::filter_state_for_viewer(runner.state(), seat),
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        }
        .waiting_for
        else {
            panic!("public permission remains available");
        };
        assert_eq!(player, P0);
        assert_eq!(remaining_casts, Some(2));
        assert_eq!(remaining_mv_budget, Some(6));
        assert_eq!(enters_with_modifications.modifications.len(), 1);
        assert!(wire.to_string().contains("EpicCopy"));
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
}

#[test]
fn departure_stateless_spell_history_thresholds_refuse_before_production_and_keep_matching_fixed_twins(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    for owner in 0..5 {
        for recursive in [false, true] {
            for addressed in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
                let victims = vec![
                    scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
                    scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
                ];
                let prior = scenario
                    .add_spell_to_hand(P0, "Supplemental matching earlier cast", true)
                    .with_mana_cost(ManaCost::generic(5))
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::NoOp,
                    ))
                    .id();
                let Effect::GainLife {
                    amount: captured, ..
                } = *departure_gain_definition(7, P0).effect
                else {
                    unreachable!()
                };
                let threshold = if addressed {
                    if recursive {
                        QuantityExpr::Sum {
                            exprs: vec![
                                QuantityExpr::Fixed { value: 0 },
                                QuantityExpr::Offset {
                                    inner: Box::new(captured),
                                    offset: 0,
                                },
                            ],
                        }
                    } else {
                        captured
                    }
                } else {
                    QuantityExpr::Fixed { value: 5 }
                };
                let mut property = FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: threshold,
                };
                if recursive {
                    property = FilterProp::Not {
                        prop: Box::new(FilterProp::Not {
                            prop: Box::new(FilterProp::AnyOf {
                                props: vec![property],
                            }),
                        }),
                    };
                }
                let mut typed = TypedFilter::new(TypeFilter::Instant);
                typed.properties.push(property);
                let mut filter = TargetFilter::Typed(typed);
                if recursive {
                    filter = TargetFilter::And {
                        filters: vec![TargetFilter::Or {
                            filters: vec![TargetFilter::Not {
                                filter: Box::new(TargetFilter::Not {
                                    filter: Box::new(filter),
                                }),
                            }],
                        }],
                    };
                }
                if owner == 2 {
                    filter = TargetFilter::with_own_cast_exclusion(Some(filter));
                }
                let reference = match owner {
                    0 => {
                        let journal = CardTypeSetSource::TurnJournal {
                            journal: TurnJournalKind::SpellsCast,
                            scope: CountScope::Controller,
                            filter: Some(filter),
                        };
                        QuantityRef::DistinctCardTypes {
                            source: if recursive {
                                CardTypeSetSource::any_of(vec![journal.clone(), journal]).unwrap()
                            } else {
                                journal
                            },
                        }
                    }
                    1 | 2 => QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(filter),
                    },
                    3 => QuantityRef::SpellsCastBeforeTriggeringSpell {
                        scope: CountScope::Controller,
                        filter: Some(filter),
                    },
                    4 => QuantityRef::SpellsCastThisGame {
                        scope: CountScope::Controller,
                        filter: Some(filter),
                    },
                    _ => unreachable!(),
                };
                let mut amount = QuantityExpr::Ref { qty: reference };
                if recursive {
                    amount = QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Sum {
                            exprs: vec![QuantityExpr::Fixed { value: 0 }, amount],
                        }),
                        offset: 0,
                    };
                }
                let mut root = departure_exile_definition(7);
                let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
                    unreachable!()
                };
                *target = TargetFilter::Or {
                    filters: victims
                        .iter()
                        .map(|id| TargetFilter::SpecificObject { id: *id })
                        .collect(),
                };
                root.sub_ability = Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        player: TargetFilter::Controller,
                        amount,
                    },
                )));
                let spell = scenario
                    .add_spell_to_hand(P0, "Supplemental history root", false)
                    .with_mana_cost(ManaCost::generic(1))
                    .with_ability_definition(if owner == 3 {
                        AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)
                    } else {
                        root.clone()
                    })
                    .id();
                if owner == 3 {
                    // A real SpellCast event supplies the before-trigger boundary.
                    let mut trigger =
                        TriggerDefinition::new(engine::types::triggers::TriggerMode::SpellCast)
                            .execute(root.clone());
                    let mut current = TypedFilter::new(TypeFilter::Sorcery);
                    current.properties.push(FilterProp::Cmc {
                        comparator: Comparator::EQ,
                        value: QuantityExpr::Fixed { value: 1 },
                    });
                    trigger.valid_card = Some(TargetFilter::Typed(current));
                    scenario
                        .add_artifact_from_oracle(P0, "Supplemental history trigger", "")
                        .with_trigger_definition(trigger);
                }
                scenario.with_mana_pool(
                    P0,
                    (0..6)
                        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
                        .collect(),
                );
                let mut runner = scenario.build();
                runner.cast(prior).resolve();
                assert_eq!(runner.state().objects[&prior].zone, Zone::Graveyard);
                assert_eq!(runner.state().players[0].mana_pool.total(), 1);
                assert!(runner.state().spells_cast_this_turn_by_player[&P0]
                    .iter()
                    .any(|record| record.spell_object_id == Some(prior) && record.mana_value == 5));
                assert!(runner.state().spells_cast_this_game_by_player[&P0]
                    .iter()
                    .any(|record| record.spell_object_id == Some(prior) && record.mana_value == 5));
                if addressed {
                    let mut state = runner.state().clone();
                    let before = serde_json::to_value(&state).unwrap();
                    let mut events = vec![];
                    let error = resolve_ability_chain(
                        &mut state,
                        &build_resolved_from_def(&root, spell, P0),
                        &mut events,
                        0,
                    )
                    .unwrap_err();
                    assert!(
                        format!("{error:?}").contains("executable context"),
                        "history owner {owner}: {error:?}"
                    );
                    assert_eq!(serde_json::to_value(&state).unwrap(), before);
                    assert!(events.is_empty());
                }
                runner.cast(spell).resolve();
                assert_eq!(
                    runner.state().players[0].mana_pool.total(),
                    0,
                    "public cast pays its real cost before resolution admission"
                );
                for victim in &victims {
                    assert_eq!(
                        runner.state().objects[victim].zone,
                        if addressed {
                            Zone::Battlefield
                        } else {
                            Zone::Exile
                        }
                    );
                }
                if addressed {
                    assert_eq!(runner.state().next_return_result_process_id, 1);
                }
                departure_assert_program_complete(&runner, spell, if addressed { 0 } else { 1 }, 0);
            }
        }
    }
}

#[test]
fn departure_perpetual_grant_refuses_caller_import_before_install_and_activates_its_complete_independent_body(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ReturnResultProcessId, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    for imported in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
        let victims = vec![
            scenario.add_creature(P0, "Centaur Courser", 3, 3).id(),
            scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
        ];
        let recipient = scenario
            .add_artifact_from_oracle(P0, "Supplemental perpetual recipient", "")
            .id();
        let mut body = if imported {
            departure_gain_definition(7, P0)
        } else {
            let mut snapshot =
                build_resolved_from_def(&departure_selected_local_body(true, false), recipient, P0);
            snapshot.return_result_process = Some(ReturnResultProcessId(901));
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::EpicCopy {
                    spell: Box::new(snapshot),
                },
            )
        };
        body.kind = AbilityKind::Activated;
        let mut root = departure_exile_definition(7);
        let Effect::ChangeZoneAll { target, .. } = root.effect.as_mut() else {
            unreachable!()
        };
        *target = TargetFilter::Or {
            filters: victims
                .iter()
                .map(|id| TargetFilter::SpecificObject { id: *id })
                .collect(),
        };
        root.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ApplyPerpetual {
                target: TargetFilter::SpecificObject { id: recipient },
                modification: PerpetualModification::GrantAbility {
                    modifications: vec![PerpetualGrantModification::GrantAbility {
                        definition: Box::new(body.clone()),
                    }],
                },
            },
        )));
        let spell = scenario
            .add_spell_to_hand(P0, "Supplemental perpetual installer", false)
            .with_mana_cost(ManaCost::generic(1))
            .with_ability_definition(root.clone())
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        if imported {
            let mut state = runner.state().clone();
            let before = serde_json::to_value(&state).unwrap();
            let mut events = vec![];
            let error = resolve_ability_chain(
                &mut state,
                &build_resolved_from_def(&root, spell, P0),
                &mut events,
                0,
            )
            .unwrap_err();
            assert!(format!("{error:?}").contains("foreign instruction-result dependency"));
            assert_eq!(serde_json::to_value(&state).unwrap(), before);
            assert!(events.is_empty());
        }
        runner.cast(spell).resolve();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        departure_assert_program_complete(&runner, spell, 0, 0);
        let object = &runner.state().objects[&recipient];
        if imported {
            for victim in victims {
                assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
            }
            assert!(object.perpetual_mods.is_empty());
            assert!(object.abilities.is_empty());
            assert!(object.base_abilities.is_empty());
            assert_eq!(runner.state().next_return_result_process_id, 1);
            continue;
        }
        for victim in victims {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
        }
        assert_eq!(object.abilities.len(), 1);
        assert_eq!(object.base_abilities.len(), 1);
        assert_eq!(object.perpetual_mods.len(), 1);
        assert_eq!(object.abilities[0], body);
        assert_eq!(object.base_abilities[0], body);
        let saved = serde_json::to_value(runner.state()).unwrap();
        assert!(saved["objects"]
            .to_string()
            .contains("\"return_result_process\":901"));
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        departure_assert_private_result_views(runner.state());
        let next_process = runner.state().next_return_result_process_id;
        runner.activate(recipient, 0).resolve();
        let WaitingFor::ChooseOneOfBranch {
            player,
            branches,
            expected_return_result_process,
            return_result_occurrence,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("public activation and the real Epic copy reach the local R11 reader choice")
        };
        assert_eq!(*player, P0);
        assert_eq!(branches.len(), 2);
        let process = expected_return_result_process.unwrap();
        assert_ne!(process, ReturnResultProcessId(901));
        assert!(process.0 >= next_process);
        assert_eq!(
            runner.state().return_result_frames[&return_result_occurrence.unwrap()].process,
            Some(process)
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        departure_assert_private_result_views(runner.state());
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        departure_assert_program_complete(&runner, spell, 3, 0);
        assert_eq!(runner.state().objects[&recipient].abilities.len(), 1);
        assert_eq!(runner.state().objects[&recipient].base_abilities.len(), 1);
    }
}

#[test]
fn departure_actual_emitted_mana_units_keep_grants_order_and_pips_while_all_views_redact_private_bodies(
) {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaSpellGrant, ManaType};
    let (mut runner, spell, subjects) =
        departure_public_program_runner(false, false, 2, |scenario| {
            let marker = scenario
                .add_spell_to_hand(P0, "Supplemental usable grant-bearing mana", false)
                .with_mana_cost(ManaCost::generic(2))
                .with_ability_definition(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
                .id();
            let keyword = Keyword::CumulativeUpkeep(departure_independent_epic_cost(0));
            let filter = TargetFilter::Typed({
                let mut typed = TypedFilter::default();
                typed.properties.push(FilterProp::WithKeyword {
                    value: keyword.clone(),
                });
                typed
            });
            let grants = vec![
                ManaSpellGrant::CantBeCountered {
                    filter: filter.clone(),
                },
                ManaSpellGrant::EntersWithCounters {
                    filter,
                    counter_type: engine::types::counter::CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                },
                ManaSpellGrant::AddKeywordUntilEndOfTurn {
                    keyword,
                    restriction: None,
                    duration: Box::new(Duration::UntilEndOfTurn),
                },
                ManaSpellGrant::TriggerOnSpend {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                    ability: Box::new(departure_retained_epic_definition(0)),
                },
            ];
            (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green, ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants,
                        expiry: None,
                        target: None,
                    },
                ),
                vec![marker],
            )
        });
    departure_assert_program_complete(&runner, spell, 0, 0);
    let units: Vec<_> = runner.state().players[0].mana_pool.units().collect();
    assert_eq!(units.len(), 2);
    assert_eq!(
        units.iter().map(|unit| unit.color).collect::<Vec<_>>(),
        vec![ManaType::Green, ManaType::Blue]
    );
    assert!(units
        .iter()
        .all(|unit| unit.grants.len() == 4 && unit.source_id == spell && unit.pip_id.0 != 0));
    assert_ne!(units[0].pip_id, units[1].pip_id);
    assert!(
        serde_json::to_string(&units)
            .unwrap()
            .contains("\"return_result_process\":901"),
        "actual production copied the independent snapshots into real pool units"
    );
    let authoritative = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
    departure_assert_private_result_views(runner.state());
    for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
        let projected = match viewer {
            Some(seat) => engine::game::visibility::filter_state_for_viewer(runner.state(), seat),
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        };
        let public_units: Vec<_> = projected.players[0].mana_pool.units().collect();
        assert_eq!(public_units.len(), units.len());
        for (before, after) in units.iter().zip(&public_units) {
            assert_eq!(before.pip_id, after.pip_id);
            assert_eq!(before.color, after.color);
            assert_eq!(before.source_id, after.source_id);
            assert_eq!(before.grants.len(), after.grants.len());
            assert_eq!(
                serde_json::to_value(&before.restrictions).unwrap(),
                serde_json::to_value(&after.restrictions).unwrap()
            );
            assert!(!serde_json::to_string(&after.grants)
                .unwrap()
                .contains("return_result_process"));
        }
        let wire = serde_json::to_value(ClientGameStateRef::wrap(runner.state(), viewer)).unwrap();
        let plain = serde_json::to_value(&projected).unwrap();
        assert_eq!(
            wire["state"]["players"][0]["mana_pool"],
            plain["players"][0]["mana_pool"]
        );
    }
    assert_eq!(
        serde_json::to_value(runner.state()).unwrap(),
        authoritative,
        "six projections preserve authoritative grant snapshots before any copy clearing"
    );
    runner.cast(subjects[0]).resolve();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the two actual retained grant-bearing units pay the real next spell cost"
    );
    assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Graveyard);
    departure_assert_program_complete(&runner, spell, 0, 0);
}

#[test]
fn departure_public_history_lki_and_chosen_keyword_custody_keeps_public_spine_without_private_processes(
) {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::counter::CounterType;
    use engine::types::game_state::{
        AttackDeclarationRecord, BattlefieldEntryRecord, CounterAddedRecord, NextSpellModifier,
        PendingNextSpellModifier,
    };
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    let keyword = Keyword::CumulativeUpkeep(departure_independent_epic_cost(0));
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let public_source = scenario
        .add_artifact_from_oracle(P0, "Supplemental public record source", "")
        .with_keyword(keyword.clone())
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental actual keyword history producer", true)
        .with_keyword(keyword.clone())
        .with_mana_cost(ManaCost::generic(1))
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
        ))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    departure_assert_program_complete(&runner, spell, -1, 0);
    assert!(runner.state().spells_cast_this_turn_by_player[&P0]
        .iter()
        .any(|record| record.spell_object_id == Some(spell) && record.keywords.contains(&keyword)));
    assert!(runner.state().spells_cast_this_game_by_player[&P0]
        .iter()
        .any(|record| record.spell_object_id == Some(spell) && record.keywords.contains(&keyword)));
    assert!(runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .any(|record| record.source_id == spell && record.source_keywords.contains(&keyword)));
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .any(|record| record.object_id == spell && record.keywords.contains(&keyword)));
    // These remaining roots are explicit typed custody supplements. The above
    // public cast/damage/zone pipelines are the actual emission witnesses.
    let object = runner.state_mut().objects.get_mut(&public_source).unwrap();
    object
        .chosen_attributes
        .push(ChosenAttribute::Keyword(keyword.clone()));
    object.cast_spell_keywords.push(keyword.clone());
    let snapshot = object.snapshot_for_mana_spent();
    let record =
        object.snapshot_for_zone_change(public_source, Some(Zone::Battlefield), Zone::Exile);
    let incarnation = object.incarnation;
    object.cast_cost_paid_object = Some(CostPaidObjectSnapshot::capture(object, snapshot.clone()));
    runner.state_mut().players[0]
        .chosen_attributes
        .push(ChosenAttribute::Keyword(keyword.clone()));
    runner.state_mut().last_named_choice = Some(ChoiceValue::Keyword(keyword.clone()));
    runner
        .state_mut()
        .pending_next_spell_modifiers
        .push(PendingNextSpellModifier {
            player: P0,
            modifier: NextSpellModifier::HasKeyword {
                keyword: keyword.clone(),
            },
            spell_filter: None,
            source_id: Some(public_source),
        });
    runner
        .state_mut()
        .lki_cache
        .insert(public_source, snapshot.clone());
    runner
        .state_mut()
        .lki_by_incarnation
        .entry(public_source)
        .or_default()
        .insert(incarnation, snapshot.clone());
    runner
        .state_mut()
        .zone_changes_this_turn
        .push_back(record.clone());
    runner
        .state_mut()
        .sacrificed_permanents_this_turn
        .push_back(record.clone());
    runner
        .state_mut()
        .created_tokens_this_turn
        .push_back(record);
    runner
        .state_mut()
        .battlefield_entries_this_turn
        .push_back(BattlefieldEntryRecord {
            object_id: public_source,
            name: snapshot.name.clone(),
            core_types: snapshot.card_types.clone(),
            subtypes: snapshot.subtypes.clone(),
            supertypes: snapshot.supertypes.clone(),
            colors: snapshot.colors.clone(),
            keywords: vec![keyword.clone()],
            controller: P0,
        });
    runner
        .state_mut()
        .counter_added_this_turn
        .push_back(CounterAddedRecord {
            actor: P0,
            object_id: public_source,
            counter_type: CounterType::Plus1Plus1,
            count: 1,
            name: snapshot.name.clone(),
            core_types: snapshot.card_types.clone(),
            subtypes: snapshot.subtypes.clone(),
            supertypes: snapshot.supertypes.clone(),
            keywords: vec![keyword.clone()],
            power: snapshot.power,
            toughness: snapshot.toughness,
            colors: snapshot.colors.clone(),
            mana_value: snapshot.mana_value,
            controller: P0,
            owner: P0,
            counters: Default::default(),
        });
    runner
        .state_mut()
        .attacker_declarations_this_turn
        .push(AttackDeclarationRecord {
            object_id: public_source,
            incarnation: Some(incarnation),
            lki: snapshot,
            is_token: false,
            is_commander: false,
        });
    let authoritative = serde_json::to_value(runner.state()).unwrap();
    assert!(authoritative
        .to_string()
        .contains("\"return_result_process\":901"));
    *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
    departure_assert_private_result_views(runner.state());
    for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
        let projected = match viewer {
            Some(seat) => engine::game::visibility::filter_state_for_viewer(runner.state(), seat),
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        };
        assert_eq!(
            projected.objects[&public_source].name,
            "Supplemental public record source"
        );
        assert_eq!(projected.objects[&public_source].zone, Zone::Battlefield);
        assert_eq!(projected.objects[&public_source].keywords.len(), 1);
        assert_eq!(projected.objects[&public_source].chosen_attributes.len(), 1);
        assert_eq!(projected.players[0].chosen_attributes.len(), 1);
        assert_eq!(projected.pending_next_spell_modifiers.len(), 1);
        assert_eq!(
            projected.lki_cache[&public_source].name,
            "Supplemental public record source"
        );
        assert_eq!(
            projected.lki_by_incarnation[&public_source][&incarnation]
                .keywords
                .len(),
            1
        );
        for public in [
            serde_json::to_value(&projected).unwrap(),
            serde_json::to_value(ClientGameStateRef::wrap(runner.state(), viewer)).unwrap()
                ["state"]
                .clone(),
        ] {
            for field in [
                "spells_cast_this_turn_by_player",
                "spells_cast_this_game_by_player",
                "zone_changes_this_turn",
                "sacrificed_permanents_this_turn",
                "created_tokens_this_turn",
                "battlefield_entries_this_turn",
                "damage_dealt_this_turn",
                "counter_added_this_turn",
                "attacker_declarations_this_turn",
                "lki_cache",
                "lki_by_incarnation",
            ] {
                assert_eq!(
                    public[field].to_string().matches("EpicCopy").count(),
                    authoritative[field].to_string().matches("EpicCopy").count(),
                    "public record/keyword payload retained at {field}"
                );
            }
        }
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
    departure_assert_program_complete(&runner, spell, -1, 0);
}

// Supplemental canonical payload constructors: no printed-card or Epic-payment claim.
fn departure_opaque_epic_cost(mode: u8) -> engine::types::ability::AbilityCost {
    use engine::types::ability::*;
    if mode != 4 {
        return departure_independent_epic_cost(mode);
    }
    let mut body = departure_exile_definition(7);
    body.sub_ability = Some(Box::new(departure_gain_definition(7, PlayerId(0))));
    let mut snapshot = build_resolved_from_def(&body, ObjectId(0), PlayerId(0));
    snapshot.return_result_process = Some(engine::types::game_state::ReturnResultProcessId(901));
    AbilityCost::EffectCost {
        effect: Box::new(Effect::EpicCopy {
            spell: Box::new(snapshot),
        }),
    }
}

fn departure_opaque_token_spec() -> engine::types::proposed_event::TokenSpec {
    use engine::types::proposed_event::*;
    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: "Supplemental retained Food".into(),
            power: None,
            toughness: None,
            loyalty: None,
            core_types: vec![engine::types::card_type::CoreType::Artifact],
            subtypes: vec!["Food".into()],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
        },
        script_name: "Supplemental retained Food".into(),
        static_abilities: vec![],
        enter_with_counters: vec![],
        tapped: false,
        enters_attacking: false,
        sacrifice_at: None,
        source_id: ObjectId(0),
        controller: PlayerId(0),
        attach_to: TokenHostRequest::NotRequested,
    }
}

fn departure_opaque_filter(mode: u8) -> engine::types::ability::TargetFilter {
    use engine::types::ability::*;
    TargetFilter::Typed({
        let mut filter = TypedFilter::default();
        filter.properties.push(FilterProp::WithKeyword {
            value: engine::types::keywords::Keyword::CumulativeUpkeep(departure_opaque_epic_cost(
                mode,
            )),
        });
        filter
    })
}

fn departure_opaque_conditions(mode: u8) -> Vec<engine::types::ability::DelayedTriggerCondition> {
    use engine::types::ability::*;
    use engine::types::phase::Phase;
    use engine::types::triggers::TriggerMode;
    let filter = departure_opaque_filter(mode);
    let mut matcher = TriggerDefinition::new(TriggerMode::SpellCast).valid_card(filter.clone());
    matcher.unless_pay = Some(UnlessPayModifier {
        cost: departure_opaque_epic_cost(mode),
        payer: filter.clone(),
    });
    vec![
        DelayedTriggerCondition::WhenDies {
            filter: filter.clone(),
        },
        DelayedTriggerCondition::WhenLeavesPlayFiltered {
            filter: filter.clone(),
        },
        DelayedTriggerCondition::WhenEntersBattlefield {
            filter: filter.clone(),
        },
        DelayedTriggerCondition::WhenDiesOrExiled { filter },
        DelayedTriggerCondition::WheneverEvent {
            trigger: Box::new(matcher.clone()),
            expiry: Default::default(),
        },
        DelayedTriggerCondition::WhenNextEvent {
            trigger: Box::new(matcher.clone()),
            or_trigger: None,
            lifetime: Default::default(),
        },
        DelayedTriggerCondition::WhenNextEvent {
            trigger: Box::new(TriggerDefinition::new(TriggerMode::SpellCast)),
            or_trigger: Some(Box::new(matcher)),
            lifetime: Default::default(),
        },
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::End,
            player: PlayerId(0),
            gate: Default::default(),
            binding: Default::default(),
        },
        DelayedTriggerCondition::AtBeginningOfAddedPhase {
            phase: Phase::BeginCombat,
            entry: None,
        },
        DelayedTriggerCondition::WhenLeavesPlay {
            object_id: ObjectId(0),
        },
    ]
}

fn departure_opaque_payloads(
    mode: u8,
    face: &engine::game::game_object::BackFaceData,
) -> Vec<engine::types::ability::Effect> {
    use engine::types::ability::*;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::triggers::TriggerMode;
    use std::sync::Arc;
    let keyword =
        engine::types::keywords::Keyword::CumulativeUpkeep(departure_opaque_epic_cost(mode));
    let body = if mode == 4 {
        let mut body = departure_exile_definition(7);
        body.sub_ability = Some(Box::new(departure_gain_definition(7, PlayerId(0))));
        body
    } else if mode == 3 {
        departure_gain_definition(7, PlayerId(0))
    } else if mode == 0 {
        departure_selected_local_body(false, false)
    } else {
        let mut body = departure_gain_definition(7, PlayerId(0));
        let Effect::GainLife { amount, .. } = body.effect.as_mut() else {
            unreachable!()
        };
        if mode == 1 {
            let QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate { source, .. },
            } = amount
            else {
                unreachable!()
            };
            *source = ZoneChangeRecordSource::ThisTurn;
        } else {
            *amount = QuantityExpr::Fixed { value: 5 };
        }
        body
    };
    let trigger = TriggerDefinition::new(TriggerMode::SpellCast).execute(body.clone());
    let mut nested_replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone);
    nested_replacement.execute = Some(Box::new(body.clone()));
    let mods = vec![
        ContinuousModification::GrantAbility {
            definition: Box::new(body.clone()),
        },
        ContinuousModification::GrantTrigger {
            trigger: Box::new(trigger),
        },
        ContinuousModification::GrantReplacement {
            replacement: Box::new(nested_replacement),
        },
        ContinuousModification::GrantStaticAbility {
            definition: Box::new(StaticDefinition::new(StaticMode::Continuous).modifications(
                vec![ContinuousModification::GrantAbility {
                    definition: Box::new(body),
                }],
            )),
        },
        ContinuousModification::CopyValues {
            values: Box::new({
                let mut values = departure_retained_epic_values(2);
                values.abilities = Arc::new(vec![]);
                values.keywords = vec![keyword.clone()];
                values
            }),
            display: engine::game::game_object::DisplayIdentity {
                display_source: Default::default(),
                printed_ref: None,
                token_image_ref: None,
                token_art: None,
            },
        },
    ];
    let mut effects = vec![];
    for ensure in [false, true] {
        for modification in mods.clone() {
            let mut spec = departure_opaque_token_spec();
            spec.static_abilities.push(
                StaticDefinition::new(StaticMode::Continuous).modifications(vec![modification]),
            );
            let mut replacement = ReplacementDefinition::new(ReplacementEvent::CreateToken);
            if ensure {
                replacement.ensure_token_specs = Some(vec![spec]);
            } else {
                replacement.additional_token_spec = Some(Box::new(spec));
            }
            effects.push(Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            });
        }
    }
    for leaf in 0..3 {
        let mut values = departure_retained_epic_values(2);
        values.abilities = Arc::new(vec![]);
        if leaf == 0 {
            values.keywords = vec![keyword.clone()];
        } else {
            let mut copied_face = face.clone();
            copied_face.abilities.clear();
            copied_face.keywords.clear();
            if leaf == 1 {
                let AbilityCost::EffectCost { effect } = departure_opaque_epic_cost(mode) else {
                    unreachable!()
                };
                copied_face
                    .abilities
                    .push(AbilityDefinition::new(AbilityKind::Activated, *effect));
            } else {
                copied_face.additional_cost =
                    Some(AdditionalCost::Required(departure_opaque_epic_cost(mode)));
            }
            values.prepare_face = Some(Arc::new(copied_face));
        }
        effects.push(Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::Continuous).modifications(
                vec![ContinuousModification::CopyValues {
                    values: Box::new(values),
                    display: engine::game::game_object::DisplayIdentity {
                        display_source: Default::default(),
                        printed_ref: None,
                        token_image_ref: None,
                        token_art: None,
                    },
                }],
            )],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
            end_cost: None,
        });
    }
    for condition in departure_opaque_conditions(mode) {
        effects.push(Effect::CreateDelayedTrigger {
            condition: condition.clone(),
            effect: Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
            uses_tracked_set: false,
        });
        effects.push(Effect::ExileResolvingSpellInsteadOfGraveyard {
            on_exile: Some(ExiledSpellRider::ReturnTo {
                destination: Zone::Hand,
                timing: condition,
            }),
        });
    }
    effects
}

#[test]
fn departure_opaque_installed_payloads_refuse_before_any_producer_and_preserve_independent_controls(
) {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::*;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    for mode in 0..5 {
        let mut fixture = GameScenario::new();
        let face_source = fixture
            .add_artifact_from_oracle(P0, "Supplemental face source", "")
            .id();
        let fixture = fixture.build();
        let face = engine::game::printed_cards::snapshot_object_face(
            &fixture.state().objects[&face_source],
        );
        for (slot, effect) in departure_opaque_payloads(mode, &face)
            .into_iter()
            .enumerate()
        {
            let mut scenario = GameScenario::new();
            scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
            let victim = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
            let mut root = departure_exile_definition(7);
            root.sub_ability = Some(Box::new(AbilityDefinition::new(AbilityKind::Spell, effect)));
            let spell = scenario
                .add_spell_to_hand(P0, "Supplemental opaque installer", false)
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(root.clone())
                .id();
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
            );
            let mut runner = scenario.build();
            let mut state = runner.state().clone();
            let before = serde_json::to_value(&state).unwrap();
            let mut events = vec![];
            let result = resolve_ability_chain(
                &mut state,
                &build_resolved_from_def(&root, spell, P0),
                &mut events,
                0,
            );
            // Last eight slots are four body-free condition variants on both routes.
            let rejects = mode == 3 && slot < 27;
            if rejects {
                assert!(
                    format!("{:?}", result.unwrap_err())
                        .contains("foreign instruction-result dependency"),
                    "opaque slot {slot}"
                );
                assert_eq!(serde_json::to_value(&state).unwrap(), before);
                assert!(events.is_empty());
                runner.cast(spell).resolve();
                assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
                assert_eq!(runner.state().next_return_result_process_id, 1);
                assert!(runner.state().pending_damage_replacements.is_empty());
                assert!(runner.state().delayed_triggers.is_empty());
                assert!(runner.state().transient_continuous_effects.is_empty());
                departure_assert_program_complete(&runner, spell, 0, 0);
            } else {
                // Typed admission supplement; public real installation positives follow separately.
                result.unwrap_or_else(|error| panic!("opaque slot {slot}, mode {mode}: {error:?}"));
                assert_eq!(state.objects[&victim].zone, Zone::Exile);
            }
        }
    }
}

#[test]
fn departure_both_replacement_token_specs_install_real_later_triggers_with_private_prototypes() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ReturnResultProcessId, WaitingFor};
    use engine::types::keywords::Keyword;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::triggers::TriggerMode;
    for ensure in [false, true] {
        let (mut runner, spell, subjects) =
            departure_public_program_runner(false, false, 2, |scenario| {
                let mut spec = departure_opaque_token_spec();
                spec.characteristics
                    .keywords
                    .push(Keyword::CumulativeUpkeep(departure_independent_epic_cost(
                        0,
                    )));
                let trigger = TriggerDefinition::new(TriggerMode::SpellCast)
                    .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)))
                    .execute(departure_selected_local_body(true, false));
                spec.static_abilities.push(
                    StaticDefinition::new(StaticMode::Continuous)
                        .affected(TargetFilter::SelfRef)
                        .modifications(vec![
                            ContinuousModification::GrantTrigger {
                                trigger: Box::new(trigger),
                            },
                            ContinuousModification::GrantAbility {
                                definition: Box::new(departure_retained_epic_definition(0)),
                            },
                        ]),
                );
                // Retained duration matcher/cost/filter custody is tested before the token event;
                // no claim that UntilEvent is a newly implemented token-sacrifice scheduler.
                spec.sacrifice_at = Some(Duration::UntilEvent {
                    event: Box::new({
                        let mut event = TriggerDefinition::new(TriggerMode::SpellCast)
                            .valid_card(departure_opaque_filter(0));
                        event.unless_pay = Some(UnlessPayModifier {
                            cost: departure_independent_epic_cost(0),
                            payer: departure_opaque_filter(0),
                        });
                        event
                    }),
                });
                let mut replacement = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                    .token_owner_scope(ControllerRef::You);
                if ensure {
                    replacement.ensure_token_specs = Some(vec![spec]);
                } else {
                    replacement.additional_token_spec = Some(Box::new(spec));
                }
                let token = scenario
                    .add_spell_to_hand(P0, "Supplemental real token event", false)
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Token {
                            name: "Supplemental Seed".into(),
                            power: PtValue::Fixed(0),
                            toughness: PtValue::Fixed(0),
                            types: vec!["Artifact".into(), "Treasure".into()],
                            colors: vec![],
                            keywords: vec![],
                            tapped: false,
                            count: QuantityExpr::Fixed { value: 1 },
                            owner: TargetFilter::Controller,
                            attach_to: None,
                            enters_attacking: false,
                            supertypes: vec![],
                            static_abilities: vec![],
                            enter_with_counters: vec![],
                        },
                    ))
                    .id();
                let marker = scenario
                    .add_spell_to_hand(P0, "Supplemental later sorcery", false)
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::NoOp,
                    ))
                    .id();
                (
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::AddTargetReplacement {
                            replacement: Box::new(replacement),
                            target: TargetFilter::None,
                        },
                    ),
                    vec![token, marker],
                )
            });
        departure_assert_program_complete(&runner, spell, 0, 0);
        assert_eq!(runner.state().pending_damage_replacements.len(), 1);
        let installed = &runner.state().pending_damage_replacements[0];
        let spec = if ensure {
            &installed.ensure_token_specs.as_ref().unwrap()[0]
        } else {
            installed.additional_token_spec.as_ref().unwrap()
        };
        assert_eq!(spec.characteristics.keywords.len(), 1);
        assert_eq!(spec.static_abilities.len(), 1);
        let authoritative = serde_json::to_value(runner.state()).unwrap();
        assert!(authoritative["pending_damage_replacements"]
            .to_string()
            .contains("\"return_result_process\":901"));
        *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
        departure_assert_private_result_views(runner.state());
        for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
            let projected = match viewer {
                Some(seat) => {
                    engine::game::visibility::filter_state_for_viewer(runner.state(), seat)
                }
                None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
            };
            let plain = serde_json::to_value(&projected).unwrap();
            let wire = serde_json::to_value(engine::game::derived_views::ClientGameStateRef::wrap(
                runner.state(),
                viewer,
            ))
            .unwrap();
            for view in [&plain, &wire["state"]] {
                let field = &view["pending_damage_replacements"];
                assert_eq!(field.as_array().unwrap().len(), 1);
                assert_eq!(
                    field.to_string().matches("EpicCopy").count(),
                    authoritative["pending_damage_replacements"]
                        .to_string()
                        .matches("EpicCopy")
                        .count()
                );
                let value = if ensure {
                    &field[0]["ensure_token_specs"][0]
                } else {
                    &field[0]["additional_token_spec"]
                };
                assert_eq!(
                    value["characteristics"]["display_name"],
                    "Supplemental retained Food"
                );
                assert_eq!(
                    value["characteristics"]["keywords"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(value["static_abilities"].as_array().unwrap().len(), 1);
                assert!(!value["sacrifice_at"].is_null());
            }
        }
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
        runner.cast(subjects[0]).resolve();
        let token = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == "Supplemental retained Food")
            .expect("real replacement creates its retained token");
        assert_eq!(runner.state().objects[&token].keywords.len(), 1);
        assert_eq!(runner.state().objects[&token].trigger_definitions.len(), 1);
        assert_eq!(
            runner.state().objects[&token].base_static_definitions.len(),
            1
        );
        departure_assert_program_complete(&runner, spell, 0, 0);
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        let next = runner.state().next_return_result_process_id;
        runner.cast(subjects[1]).resolve();
        let WaitingFor::ChooseOneOfBranch {
            player,
            branches,
            expected_return_result_process,
            return_result_occurrence,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual installed SpellCast trigger runs its independent producer and parks")
        };
        assert_eq!(*player, P0);
        assert_eq!(branches.len(), 2);
        let process = expected_return_result_process.unwrap();
        assert_ne!(process, ReturnResultProcessId(901));
        assert!(process.0 >= next);
        assert_eq!(
            runner.state().return_result_frames[&return_result_occurrence.unwrap()].process,
            Some(process)
        );
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved).unwrap();
        departure_assert_private_result_views(runner.state());
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        for _ in 0..12 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner.act(GameAction::PassPriority).unwrap();
        }
        departure_drain_retained_public_program(&mut runner);
        departure_assert_program_complete(&runner, spell, 3, 0);
        assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&subjects[1]].zone, Zone::Graveyard);
    }
}

#[test]
fn departure_layered_copy_retains_keywords_prepare_face_and_later_independent_activation() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::keywords::Keyword;
    use std::sync::Arc;
    let mut fixture = engine::game::scenario::GameScenario::new();
    let face_source = fixture
        .add_artifact_from_oracle(P0, "Supplemental prepare source", "")
        .id();
    let fixture = fixture.build();
    let mut face =
        engine::game::printed_cards::snapshot_object_face(&fixture.state().objects[&face_source]);
    face.layout_kind = Some(engine::types::card::LayoutKind::Prepare);
    face.additional_cost = Some(AdditionalCost::Required(departure_independent_epic_cost(0)));
    let artifact_types = fixture.state().objects[&face_source].card_types.clone();
    let (mut runner, spell, subjects) =
        departure_public_program_runner(false, false, 2, |scenario| {
            let recipient = scenario
                .add_artifact_from_oracle(P0, "Supplemental layered recipient", "")
                .id();
            // Typed face setup; real resolution installs both leaves before layers apply the copy.
            let mut values = departure_retained_epic_values(0);
            values.card_types = artifact_types.clone();
            values.prepare_face = Some(Arc::new(face.clone()));
            let mut snapshot =
                build_resolved_from_def(&departure_selected_local_body(true, false), recipient, P0);
            snapshot.return_result_process =
                Some(engine::types::game_state::ReturnResultProcessId(901));
            values.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::EpicCopy {
                    spell: Box::new(snapshot),
                },
            )]);
            values.keywords = vec![Keyword::CumulativeUpkeep(departure_independent_epic_cost(
                0,
            ))];
            // snapshot_object_face requires the real scenario object after build; supply it below using the existing setup callback instead.
            (
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GenericEffect {
                        static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                            .affected(TargetFilter::SpecificObject { id: recipient })
                            .modifications(vec![ContinuousModification::CopyValues {
                                values: Box::new(values),
                                display: engine::game::game_object::DisplayIdentity {
                                    display_source: Default::default(),
                                    printed_ref: None,
                                    token_image_ref: None,
                                    token_art: None,
                                },
                            }])],
                        duration: Some(Duration::UntilEndOfTurn),
                        target: None,
                        end_cost: None,
                    },
                ),
                vec![recipient],
            )
        });
    departure_assert_program_complete(&runner, spell, 0, 0);
    assert_eq!(runner.state().transient_continuous_effects.len(), 1);
    let recipient = subjects[0];
    assert_eq!(runner.state().objects[&recipient].keywords.len(), 1);
    assert_eq!(runner.state().objects[&recipient].abilities.len(), 1);
    assert!(runner.state().objects[&recipient]
        .copied_prepare_face
        .is_some());
    let saved = serde_json::to_value(runner.state()).unwrap();
    assert!(saved["transient_continuous_effects"]
        .to_string()
        .contains("\"return_result_process\":901"));
    *runner.state_mut() = serde_json::from_value(saved.clone()).unwrap();
    departure_assert_private_result_views(runner.state());
    for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
        let projected = match viewer {
            Some(seat) => engine::game::visibility::filter_state_for_viewer(runner.state(), seat),
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        };
        assert_eq!(projected.objects[&recipient].keywords.len(), 1);
        assert!(projected.objects[&recipient].copied_prepare_face.is_some());
        let plain = serde_json::to_value(&projected).unwrap();
        let wire = serde_json::to_value(engine::game::derived_views::ClientGameStateRef::wrap(
            runner.state(),
            viewer,
        ))
        .unwrap();
        for view in [&plain, &wire["state"]] {
            assert_eq!(
                view["transient_continuous_effects"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                view["transient_continuous_effects"]
                    .to_string()
                    .matches("EpicCopy")
                    .count(),
                saved["transient_continuous_effects"]
                    .to_string()
                    .matches("EpicCopy")
                    .count()
            );
        }
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), saved);
    let next = runner.state().next_return_result_process_id;
    runner.activate(recipient, 0).resolve();
    let WaitingFor::ChooseOneOfBranch {
        player,
        branches,
        expected_return_result_process,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("later independent copied ability must reach its real local choice")
    };
    assert_eq!(*player, P0);
    assert_eq!(branches.len(), 2);
    let process = expected_return_result_process.unwrap();
    assert_ne!(
        process,
        engine::types::game_state::ReturnResultProcessId(901)
    );
    assert!(process.0 >= next);
    assert!(runner
        .state()
        .return_result_frames
        .values()
        .any(|frame| frame.process == Some(process)));
    let saved = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(saved).unwrap();
    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
    departure_assert_program_complete(&runner, spell, 3, 0);
    assert!(runner.state().objects[&recipient]
        .copied_prepare_face
        .is_some());
}

#[test]
fn departure_actual_delayed_conditions_and_return_timing_keep_local_snapshots_private() {
    use engine::game::scenario::P0;
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::triggers::TriggerMode;
    // Actual installations cover the filter-bearing conditions; embedded matchers
    // carry filters (not execute bodies). All remaining variants have typed custody below.
    for route in [false, true] {
        let (mut runner, spell, subjects) =
            departure_public_program_runner(false, false, 2, |scenario| {
                let condition = DelayedTriggerCondition::WhenDies {
                    filter: departure_opaque_filter(0),
                };
                if route {
                    let marker = scenario
                        .add_spell_to_hand(P0, "Supplemental returned spell", false)
                        .with_ability_definition(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::NoOp,
                        ))
                        .id();
                    let host = scenario
                        .add_artifact_from_oracle(P0, "Supplemental ReturnTo stamper", "")
                        .with_trigger_definition(
                            TriggerDefinition::new(TriggerMode::SpellCast)
                                .valid_card(TargetFilter::SpecificObject { id: marker })
                                .execute(AbilityDefinition::new(
                                    AbilityKind::Spell,
                                    Effect::ExileResolvingSpellInsteadOfGraveyard {
                                        on_exile: Some(ExiledSpellRider::ReturnTo {
                                            destination: Zone::Hand,
                                            timing: condition,
                                        }),
                                    },
                                )),
                        )
                        .id();
                    (
                        AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                        vec![marker, host],
                    )
                } else {
                    (
                        AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::CreateDelayedTrigger {
                                condition,
                                effect: Box::new(AbilityDefinition::new(
                                    AbilityKind::Spell,
                                    Effect::NoOp,
                                )),
                                uses_tracked_set: false,
                            },
                        ),
                        vec![],
                    )
                }
            });
        departure_assert_program_complete(&runner, spell, 0, 0);
        if route {
            runner.cast(subjects[0]).commit();
            for _ in 0..12 {
                if runner.state().objects[&subjects[0]]
                    .exile_from_stack_rider
                    .is_some()
                {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(runner.state().objects[&subjects[0]].zone, Zone::Stack);
            assert!(runner.state().delayed_triggers.is_empty(), "the stamped timing is retained on the public spell before the exile replacement applies");
            assert!(matches!(
                runner.state().objects[&subjects[0]].exile_from_stack_rider,
                Some(ExiledSpellRider::ReturnTo { .. })
            ));
            let stamped = serde_json::to_value(runner.state()).unwrap();
            assert!(serde_json::to_string(
                &runner.state().objects[&subjects[0]].exile_from_stack_rider
            )
            .unwrap()
            .contains("\"return_result_process\":901"));
            *runner.state_mut() = serde_json::from_value(stamped.clone()).unwrap();
            departure_assert_private_result_views(runner.state());
            for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
                let projected = match viewer {
                    Some(seat) => {
                        engine::game::visibility::filter_state_for_viewer(runner.state(), seat)
                    }
                    None => {
                        engine::game::visibility::filter_state_for_unseated_viewer(runner.state())
                    }
                };
                let plain = serde_json::to_value(&projected).unwrap();
                let wire = serde_json::to_value(
                    engine::game::derived_views::ClientGameStateRef::wrap(runner.state(), viewer),
                )
                .unwrap();
                for view in [&plain, &wire["state"]] {
                    let object = &view["objects"][subjects[0].0.to_string()];
                    assert_eq!(object["name"], "Supplemental returned spell");
                    assert_eq!(object["zone"], "Stack");
                    assert_eq!(
                        object["exile_from_stack_rider"]
                            .to_string()
                            .matches("EpicCopy")
                            .count(),
                        1
                    );
                }
            }
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), stamped);
            for _ in 0..12 {
                if runner.state().objects[&subjects[0]].zone == Zone::Exile {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(
                runner.state().objects[&subjects[0]].zone,
                Zone::Exile,
                "real cast trigger stamps the rider, and actual resolution applies it"
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(runner.state().return_result_frames.is_empty());
        }
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        assert!(matches!(
            runner.state().delayed_triggers[0].condition,
            DelayedTriggerCondition::WhenDies { .. }
        ));
        let authoritative = serde_json::to_value(runner.state()).unwrap();
        assert!(authoritative["delayed_triggers"]
            .to_string()
            .contains("\"return_result_process\":901"));
        *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
        departure_assert_private_result_views(runner.state());
        for viewer in [Some(PlayerId(0)), Some(PlayerId(1)), None] {
            let projected = match viewer {
                Some(seat) => {
                    engine::game::visibility::filter_state_for_viewer(runner.state(), seat)
                }
                None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
            };
            let plain = serde_json::to_value(&projected).unwrap();
            let wire = serde_json::to_value(engine::game::derived_views::ClientGameStateRef::wrap(
                runner.state(),
                viewer,
            ))
            .unwrap();
            for view in [&plain, &wire["state"]] {
                assert_eq!(view["delayed_triggers"].as_array().unwrap().len(), 1);
                assert_eq!(
                    view["delayed_triggers"]
                        .to_string()
                        .matches("EpicCopy")
                        .count(),
                    authoritative["delayed_triggers"]
                        .to_string()
                        .matches("EpicCopy")
                        .count()
                );
                assert_eq!(
                    view["delayed_triggers"][0]["source_id"],
                    authoritative["delayed_triggers"][0]["source_id"]
                );
            }
        }
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
        // Explicit typed-custody supplements for all 11 shapes / 3 embedded-trigger slots.
        for condition in departure_opaque_conditions(0) {
            let mut supplement = runner.state().clone();
            supplement.delayed_triggers[0].condition = condition;
            let original = serde_json::to_value(&supplement).unwrap();
            departure_assert_private_result_views(&supplement);
            assert_eq!(serde_json::to_value(&supplement).unwrap(), original);
        }
    }
}

#[test]
fn departure_public_departed_spell_object_keeps_its_pending_cast_trigger_and_redacts_private_payload(
) {
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::keywords::Keyword;
    use engine::types::triggers::TriggerMode;
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Supplemental public countered spell", false)
        .with_keyword(Keyword::CumulativeUpkeep(departure_independent_epic_cost(
            0,
        )))
        .with_ability_definition(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
        .id();
    let watcher = scenario
        .add_artifact_from_oracle(P0, "Supplemental pending cast watcher", "")
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::SpellCast)
                .valid_card(TargetFilter::SpecificObject { id: spell })
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        )
        .id();
    scenario
        .add_artifact_from_oracle(P1, "Supplemental counter on cast", "")
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::SpellCast)
                .valid_card(TargetFilter::SpecificObject { id: spell })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::CounterAll {
                        target: TargetFilter::SpecificObject { id: spell },
                    },
                )),
        );
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    for _ in 0..12 {
        if runner.state().departed_stack_spells.contains_key(&spell) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == watcher),
        "actual cast trigger is still pending when its spell departs"
    );
    let departed = runner.state().departed_stack_spells[&spell]
        .values()
        .next()
        .unwrap();
    assert_eq!(departed.object.name, "Supplemental public countered spell");
    assert_eq!(departed.object.keywords.len(), 1);
    assert_eq!(departed.entry.id, spell);
    let authoritative = serde_json::to_value(runner.state()).unwrap();
    assert!(serde_json::to_string(&departed.object)
        .unwrap()
        .contains("\"return_result_process\":901"));
    *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
    departure_assert_private_result_views(runner.state());
    for viewer in [Some(P0), Some(P1), None] {
        let projected = match viewer {
            Some(seat) => engine::game::visibility::filter_state_for_viewer(runner.state(), seat),
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        };
        let departed = projected.departed_stack_spells[&spell]
            .values()
            .next()
            .unwrap();
        assert_eq!(departed.object.name, "Supplemental public countered spell");
        assert_eq!(departed.entry.id, spell);
        assert_eq!(departed.object.keywords.len(), 1);
        assert_eq!(departed.object.base_keywords.len(), 1);
        let plain = serde_json::to_value(&projected).unwrap();
        let wire = serde_json::to_value(engine::game::derived_views::ClientGameStateRef::wrap(
            runner.state(),
            viewer,
        ))
        .unwrap();
        for view in [&plain, &wire["state"]] {
            assert_eq!(
                view["departed_stack_spells"]
                    .to_string()
                    .matches("EpicCopy")
                    .count(),
                authoritative["departed_stack_spells"]
                    .to_string()
                    .matches("EpicCopy")
                    .count()
            );
            assert!(view["departed_stack_spells"]
                .to_string()
                .contains("Supplemental public countered spell"));
        }
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
    for _ in 0..12 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    departure_drain_retained_public_program(&mut runner);
    departure_assert_program_complete(&runner, spell, 0, 0);
}

#[test]
fn departure_actual_aura_host_pause_projects_the_parked_entrant_without_private_snapshot_authority()
{
    use engine::game::game_object::AttachTarget;
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::*;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaColor;
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let maker = scenario
        .add_artifact_from_oracle(P0, "Supplemental copy maker", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![ContinuousModification::SetColor {
                colors: vec![ManaColor::Blue],
            }],
        })
        .id();
    let hosts = [
        scenario
            .add_creature(P0, "Supplemental first host", 2, 2)
            .id(),
        scenario
            .add_creature(P0, "Supplemental second host", 2, 2)
            .id(),
    ];
    let original_host = scenario
        .add_creature(P1, "Supplemental original host", 2, 2)
        .id();
    // Established non-liminal copy fixture, with the exact complete Fylgja source
    // already used by yenna_aura_token_copy. Its enter-with-counters replacement
    // selects the real projected-entrant host-pause route; the extra keyword is typed custody.
    let aura = scenario.add_enchantment_from_oracle(P1, "Fylgja", "Enchant creature\nThis Aura enters with four healing counters on it.\nRemove a healing counter from this Aura: Prevent the next 1 damage that would be dealt to enchanted creature this turn.\n{2}{W}: Put a healing counter on this Aura.")
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))))
        .with_keyword(Keyword::CumulativeUpkeep(departure_independent_epic_cost(0))).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(original_host));
    runner.activate(maker, 0).target_object(aura).resolve();
    let WaitingFor::ReturnAsAuraTarget {
        player,
        returned_id,
        legal_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual non-liminal copy must park at the public host choice")
    };
    assert_eq!(*player, P0);
    assert_eq!(legal_targets.len(), 2);
    assert!(hosts
        .iter()
        .all(|host| legal_targets.contains(&TargetRef::Object(*host))));
    let copy = *returned_id;
    let parked = runner
        .state()
        .entering_aura_authority
        .as_ref()
        .expect("actual projected entrant is retained independently of the live object map");
    assert_eq!(parked.aura_id, copy);
    assert_eq!(parked.entrant.name, "Fylgja");
    assert_eq!(parked.entrant.color, vec![ManaColor::Blue]);
    assert!(serde_json::to_string(&parked.entrant)
        .unwrap()
        .contains("\"return_result_process\":901"));
    let authoritative = serde_json::to_value(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_value(authoritative.clone()).unwrap();
    departure_assert_private_result_views(runner.state());
    for viewer in [Some(P0), Some(P1), None] {
        let projected = match viewer {
            Some(seat) => engine::game::visibility::filter_state_for_viewer(runner.state(), seat),
            None => engine::game::visibility::filter_state_for_unseated_viewer(runner.state()),
        };
        let parked = projected.entering_aura_authority.as_ref().unwrap();
        assert_eq!(parked.aura_id, copy);
        assert_eq!(parked.entrant.name, "Fylgja");
        assert_eq!(
            parked.entrant.keywords.len(),
            runner
                .state()
                .entering_aura_authority
                .as_ref()
                .unwrap()
                .entrant
                .keywords
                .len()
        );
        let WaitingFor::ReturnAsAuraTarget {
            player,
            legal_targets,
            ..
        } = &projected.waiting_for
        else {
            panic!("public host prompt retained")
        };
        assert_eq!(*player, P0);
        assert_eq!(legal_targets.len(), 2);
        let plain = serde_json::to_value(&projected).unwrap();
        let wire = serde_json::to_value(engine::game::derived_views::ClientGameStateRef::wrap(
            runner.state(),
            viewer,
        ))
        .unwrap();
        for view in [&plain, &wire["state"]] {
            assert_eq!(view["entering_aura_authority"]["entrant"]["name"], "Fylgja");
            assert_eq!(
                view["entering_aura_authority"]
                    .to_string()
                    .matches("EpicCopy")
                    .count(),
                authoritative["entering_aura_authority"]
                    .to_string()
                    .matches("EpicCopy")
                    .count()
            );
        }
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), authoritative);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(hosts[1])),
        })
        .unwrap();
    departure_drain_retained_public_program(&mut runner);
    assert_eq!(runner.state().objects[&copy].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&copy].attached_to,
        Some(AttachTarget::Object(hosts[1]))
    );
    assert_eq!(
        runner.state().objects[&copy].counters.values().sum::<u32>(),
        4
    );
    assert!(runner.state().entering_aura_authority.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().return_result_frames.is_empty());
}

// ---------------------------------------------------------------------------
// CR 608.2e two-action order through the real cast pipeline.
// ---------------------------------------------------------------------------

/// A battlefield token's `(controller, owner, +1/+1 counters)`.
type TokenView = (PlayerId, PlayerId, u32);

/// Every battlefield token named `name`, sorted by `(owner, controller)`.
fn tokens_named(runner: &GameRunner, name: &str) -> Vec<TokenView> {
    let state = runner.state();
    let mut tokens: Vec<TokenView> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.is_token && object.name == name)
        .map(|object| {
            (
                object.controller,
                object.owner,
                object
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
            )
        })
        .collect();
    tokens.sort_by_key(|&(controller, owner, _)| (owner.0, controller.0));
    tokens
}

fn fractals(runner: &GameRunner) -> Vec<TokenView> {
    tokens_named(runner, "Fractal")
}

/// The board at one `ReplacementChoice` prompt, recorded before it is answered.
#[derive(Debug)]
struct Prompt {
    chooser: PlayerId,
    candidates: Vec<String>,
    fractals: Vec<TokenView>,
}

/// Drive the cast to an empty stack. Each `ReplacementChoice` is recorded,
/// then `at_prompt` runs (hostile board edits), then the candidate named by
/// `answer` is chosen. Any other prompt fails the test.
fn drive(
    runner: &mut GameRunner,
    answer: &dyn Fn(&[String]) -> &'static str,
    at_prompt: &mut dyn FnMut(&mut GameRunner, usize),
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return prompts,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|candidate| candidate.source_name.clone())
                    .collect();
                prompts.push(Prompt {
                    chooser: player,
                    candidates: names.clone(),
                    fractals: fractals(runner),
                });
                at_prompt(runner, prompts.len() - 1);
                let wanted = answer(&names);
                let index = names
                    .iter()
                    .position(|name| name == wanted)
                    .unwrap_or_else(|| panic!("{wanted} is not a candidate: {names:?}"));
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("replacement choice accepted");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the cast did not settle");
}

fn no_board_edit(_: &mut GameRunner, _: usize) {}

/// Two players with unequal power on the battlefield: P0 a 2/2, P1 a 3/3.
fn two_player_board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario.add_creature(P1, "Ogre", 3, 3);
    scenario
}

fn with_scales_and_evolution(scenario: &mut GameScenario, players: &[PlayerId]) {
    for &player in players {
        scenario.add_enchantment_from_oracle(player, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(player, "Branching Evolution", BRANCHING_EVOLUTION);
    }
}

/// Cast Oversimplify (P0) through the real cast pipeline.
fn cast_oversimplify(mut scenario: GameScenario) -> GameRunner {
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oversimplify", false, OVERSIMPLIFY)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    runner
}

fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "resolution completed");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn assert_counter_choice(prompt: &Prompt, chooser: PlayerId) {
    assert_eq!(prompt.chooser, chooser, "{prompt:?}");
    let mut candidates = prompt.candidates.clone();
    candidates.sort();
    assert_eq!(
        candidates,
        ["Branching Evolution", "Hardened Scales"],
        "a counter-replacement order choice: {prompt:?}"
    );
}

/// CR 608.2e: at the first counter-replacement prompt, every player has
/// already created their Fractal and none has counters yet.
fn assert_every_fractal_created_before_any_placement(prompt: &Prompt, players: &[PlayerId]) {
    let expected: Vec<TokenView> = players.iter().map(|&player| (player, player, 0)).collect();
    assert_eq!(
        prompt.fractals, expected,
        "every player's Fractal exists, owned and controlled by its creator, with no \
         counters, before any counter placement (CR 608.2e): {prompt:?}"
    );
}

/// Maintainer control (F1): a real two-player cast with unequal power, both
/// players holding Hardened Scales + Branching Evolution. Every player creates
/// before any player places (CR 608.2e), so BOTH Fractals are on the
/// battlefield at the first replacement prompt. Each affected controller orders
/// their own replacements in APNAP order (CR 616.1, CR 101.4): Hardened Scales
/// first gives (n + 1) × 2.
#[test]
fn oversimplify_creates_every_fractal_before_placing_counters_scales_first() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0, P1]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Hardened Scales", &mut no_board_edit);

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_counter_choice(&prompts[1], P1);
    assert_eq!(prompts[1].fractals, vec![(P0, P0, 6), (P1, P1, 0)]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 6), (P1, P1, 8)]);
    assert_settled(&runner);
}

/// The control with the other order: Branching Evolution first gives 2n + 1.
#[test]
fn oversimplify_creates_every_fractal_before_placing_counters_evolution_first() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0, P1]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Branching Evolution", &mut no_board_edit);

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_counter_choice(&prompts[1], P1);
    assert_eq!(prompts[1].fractals, vec![(P0, P0, 5), (P1, P1, 0)]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 5), (P1, P1, 7)]);
    assert_settled(&runner);
}

/// The pending per-player placements survive a whole-state serialization round
/// trip at the first prompt: each remaining leg keeps its bound Fractal.
#[test]
fn oversimplify_per_player_binding_survives_serialization_at_the_prompt() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0, P1]);
    let mut runner = cast_oversimplify(scenario);

    let mut round_trips = 0;
    let prompts = drive(
        &mut runner,
        &|_| "Hardened Scales",
        &mut |runner, prompt| {
            if prompt == 0 {
                let wire = serde_json::to_value(runner.state()).expect("state serializes");
                *runner.state_mut() = serde_json::from_value(wire).expect("state deserializes");
                round_trips += 1;
            }
        },
    );

    assert_eq!(
        round_trips, 1,
        "the round trip happened at the first prompt"
    );
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_counter_choice(&prompts[1], P1);
    assert_eq!(fractals(&runner), vec![(P0, P0, 6), (P1, P1, 8)]);
    assert_settled(&runner);
}

/// Three players: the counter choices run in APNAP order across more than one
/// remaining player (CR 101.4, CR 616.1), each bound to its own Fractal.
#[test]
fn oversimplify_three_players_place_counters_in_apnap_order() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Squire", 1, 1);
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.add_creature(p2, "Ogre", 3, 3);
    with_scales_and_evolution(&mut scenario, &[P0, P1, p2]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Hardened Scales", &mut no_board_edit);

    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1, p2]);
    for (prompt, chooser) in prompts.iter().zip([P0, P1, p2]) {
        assert_counter_choice(prompt, chooser);
    }
    assert_eq!(
        fractals(&runner),
        vec![(P0, P0, 4), (P1, P1, 6), (p2, p2, 8)]
    );
    assert_settled(&runner);
}

/// Only P0 holds the counter replacements: one prompt, and P1's Fractal
/// already exists when P0 chooses.
#[test]
fn oversimplify_single_affected_controller_sees_every_fractal() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Hardened Scales", &mut no_board_edit);

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 6), (P1, P1, 3)]);
    assert_settled(&runner);
}

/// Halving Season (P0) + Parallel Lives (P1): P1's creation pauses on an
/// order choice (CR 616.1).
fn creation_pause_board() -> GameScenario {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    scenario
}

/// P1's creation-order prompt, at which P0's Fractal must carry no counters:
/// no player places counters until every player has created (CR 608.2e).
fn assert_creation_choice(prompt: &Prompt) {
    assert_eq!(
        prompt.chooser, P1,
        "P1 orders its creation replacements: {prompt:?}"
    );
    let mut candidates = prompt.candidates.clone();
    candidates.sort();
    assert_eq!(
        candidates,
        ["Halving Season", "Parallel Lives"],
        "{prompt:?}"
    );
    assert_eq!(
        prompt.fractals,
        vec![(P0, P0, 0)],
        "P0 has created and placed nothing yet while P1 is still creating: {prompt:?}"
    );
}

/// A pause inside the first action: every placement waits for P1's creation.
/// Finals: P0 2; P1 creates 1 → 2 (Parallel Lives) → 1 (Halving Season) and
/// puts half of 3, rounded down, on it.
#[test]
fn oversimplify_creation_pause_defers_every_placement() {
    let mut runner = cast_oversimplify(creation_pause_board());

    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut no_board_edit);

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 2), (P1, P1, 1)]);
    assert_settled(&runner);
}

/// Both actions pause: P1's creation choice comes first, then P0's counter
/// choice — with both Fractals present and neither carrying counters.
#[test]
fn oversimplify_pauses_in_both_actions_in_rules_order() {
    let mut scenario = creation_pause_board();
    with_scales_and_evolution(&mut scenario, &[P0]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(
        &mut runner,
        &|names| {
            if names.iter().any(|name| name == "Parallel Lives") {
                "Parallel Lives"
            } else {
                "Branching Evolution"
            }
        },
        &mut no_board_edit,
    );

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    assert_counter_choice(&prompts[1], P0);
    assert_every_fractal_created_before_any_placement(&prompts[1], &[P0, P1]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 5), (P1, P1, 1)]);
    assert_settled(&runner);
}

fn p0_fractal(runner: &GameRunner) -> ObjectId {
    let state = runner.state();
    *state
        .battlefield
        .iter()
        .find(|id| {
            let object = &state.objects[*id];
            object.is_token && object.name == "Fractal" && object.owner == P0
        })
        .expect("P0's Fractal is on the battlefield")
}

/// "It" is the token the player created — the one they own (CR 111.2) — not
/// whatever they control when the counters are placed: P0's Fractal changes
/// control to P1 between the two actions and still gets P0's count.
#[test]
fn oversimplify_binds_each_player_to_the_fractal_it_owns() {
    let mut runner = cast_oversimplify(creation_pause_board());

    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut |runner, prompt| {
        if prompt == 0 {
            let fractal = p0_fractal(runner);
            let object = runner.state_mut().objects.get_mut(&fractal).unwrap();
            object.controller = P1;
            object.base_controller = Some(P1);
        }
    });

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    assert_eq!(
        fractals(&runner),
        vec![(P1, P0, 2), (P1, P1, 1)],
        "the P0-owned, P1-controlled Fractal gets P0's 2; P1's own gets P1's 1"
    );
    assert_settled(&runner);
}

/// A player whose creation was replaced with nothing has no "it": that
/// player's placement does nothing (CR 609.3), and no other token gets it.
#[test]
fn oversimplify_player_without_a_fractal_places_nothing() {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "unused", &mut no_board_edit);

    assert!(prompts.is_empty(), "{prompts:?}");
    assert_eq!(
        fractals(&runner),
        vec![(P0, P0, 2)],
        "P0's Fractal gets exactly P0's count; P1 created none"
    );
    let state = runner.state();
    let countered: Vec<&str> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.counters.values().any(|&count| count > 0))
        .map(|object| object.name.as_str())
        .collect();
    assert_eq!(countered, ["Fractal"], "no other permanent has counters");
    assert_settled(&runner);
}

/// A Fractal that leaves the battlefield between the two actions is a new
/// object (CR 400.7); its player's placement finds nothing, and the other
/// player's placement is unaffected.
#[test]
fn oversimplify_fractal_gone_before_placement_gets_nothing() {
    let mut runner = cast_oversimplify(creation_pause_board());

    let mut moved = None;
    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut |runner, prompt| {
        if prompt == 0 {
            let fractal = p0_fractal(runner);
            let mut events = Vec::new();
            move_to_zone(runner.state_mut(), fractal, Zone::Graveyard, &mut events);
            moved = Some(fractal);
        }
    });

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    let moved = moved.expect("the move happened at the prompt");
    assert!(
        !runner.state().battlefield.contains(&moved),
        "P0's Fractal left the battlefield"
    );
    assert_eq!(fractals(&runner), vec![(P1, P1, 1)]);
    assert_settled(&runner);
}

/// Tokens a replacement creates instead are the tokens the player created
/// (CR 614.1a): Divine Visitation + Parallel Lives turn P0's Fractal into two
/// Angels, and "it" is both of them.
#[test]
fn oversimplify_substituted_tokens_are_it() {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE_VISITATION);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut no_board_edit);

    assert!(prompts.is_empty(), "{prompts:?}");
    assert_eq!(
        tokens_named(&runner, "Angel"),
        vec![(P0, P0, 2), (P0, P0, 2)],
        "two Angels under P0, each with P0's count"
    );
    assert_eq!(fractals(&runner), vec![(P1, P1, 3)]);
    assert_settled(&runner);
}

/// A pause in the second action leaves a seat with no Fractal as a REMAINING
/// leg: P0's counter-order choice parks P1's placement, and P1 created none
/// (Halving Season halves P1's one Fractal to zero), so that queued leg has no
/// "it" and does nothing (CR 609.3) instead of putting P1's count on P0's
/// Fractal. P0's Fractal gets P0's 2 with Hardened Scales first:
/// (2 + 1) × 2 = 6 (Halving Season halves only an opponent's counters).
#[test]
fn oversimplify_remaining_player_without_a_fractal_places_nothing_after_a_pause() {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    with_scales_and_evolution(&mut scenario, &[P0]);
    let mut runner = cast_oversimplify(scenario);

    // Any further counter prompt (e.g. one for P1 placing on P0's Fractal) is
    // answered too, so the prompt count below is what reports it.
    let prompts = drive(
        &mut runner,
        &|names| {
            if names.iter().any(|name| name == "Hardened Scales") {
                "Hardened Scales"
            } else {
                "Branching Evolution"
            }
        },
        &mut no_board_edit,
    );

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_eq!(
        prompts[0].fractals,
        vec![(P0, P0, 0)],
        "P0 pauses on its own placement with P1's leg still remaining, and P1 \
         created no Fractal: {prompts:?}"
    );
    assert_eq!(
        fractals(&runner),
        vec![(P0, P0, 6)],
        "P0's Fractal gets exactly P0's count; P1 created none"
    );
    let state = runner.state();
    let countered: Vec<&str> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.counters.values().any(|&count| count > 0))
        .map(|object| object.name.as_str())
        .collect();
    assert_eq!(countered, ["Fractal"], "no other permanent has counters");
    assert_settled(&runner);
}

// ---------------------------------------------------------------------------
// Constructed co-scoped chains: which seats a `LastCreated` placement binds
// (CR 608.2e + CR 111.2), and what an empty seat's continuation does.
// ---------------------------------------------------------------------------

/// Constructed (synthetic) chains: "Each player creates a 1/1 Fractal token and
/// [may] put a +1/+1 counter on it[, …]", with the following instruction
/// varied. No printed card carries these shapes; they pin the per-seat binding
/// contract of the player-scope driver.
///
/// - The partition is proven only for the immediately preceding co-scoped
///   token clause (CR 608.2e + CR 111.2: "it" is the token that player
///   created, which that player owns); any other producer leaves the
///   placement reading the full latest-created set.
/// - A seat that created no token performed no placement (CR 609.3), so its
///   own "if you do" / "who doesn't" rider and "otherwise" leg read its own
///   outcome (CR 118.12, CR 608.2c), synchronously and after a pause.
/// - The instruction printed after the clause runs once after the last seat
///   (CR 608.2c), whatever link it was parsed with.
mod constructed {
    use engine::game::effects::resolve_ability_chain;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, ControllerRef, Effect, EffectOutcomeSignal, PlayerFilter,
        PtValue, QuantityExpr, ResolvedAbility, SubAbilityLink, TargetFilter, TypeFilter,
        TypedFilter, UnlessPayModifier,
    };
    use engine::types::actions::GameAction;
    use engine::types::counter::CounterType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaColor, ManaCost};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use super::{BRANCHING_EVOLUTION, HALVING_SEASON, HARDENED_SCALES, PARALLEL_LIVES};

    const P2: PlayerId = PlayerId(2);

    fn fractal_token() -> Effect {
        Effect::Token {
            name: "Fractal".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string(), "Fractal".to_string()],
            colors: vec![ManaColor::Green],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        }
    }

    fn gain(amount: i32, source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )
    }

    fn placement(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::LastCreated,
            },
            vec![],
            source,
            P0,
        )
    }

    /// The retained continuation after the placement.
    #[derive(Clone, Copy)]
    enum Rider {
        /// "Each player who doesn't gains 1 life."
        WhoDoesnt,
        /// "If they do, they gain 1 life."
        IfYouDo,
        /// "… and gain 1 life" (a dependent continuation of the same sentence).
        AndGain,
        None,
    }

    /// The board: who holds which replacement enchantment.
    #[derive(Clone, Copy)]
    enum Board {
        Empty,
        HalvingP0,
        HalvingP1,
        HalvingP2ParallelP0,
        ParallelP0,
        ScalesEvolutionP0,
    }

    /// The placement's condition.
    #[derive(Clone, Copy)]
    enum Gate {
        None,
        /// "If it's your turn" — true for the active seat only.
        YourTurn,
        /// "If you control an enchantment" — true for a seat holding one.
        ControlsEnchantment,
    }

    #[derive(Clone, Copy)]
    struct Row {
        players: u8,
        board: Board,
        active: PlayerId,
        optional: bool,
        gate: Gate,
        /// "Otherwise, gain 2 life."
        otherwise: bool,
        rider: Rider,
        /// The unscoped "You gain 3 life." printed after the clause, and the
        /// link it was parsed with.
        tail: Option<SubAbilityLink>,
        /// The seat that declines its optional placement, if any.
        decline: Option<PlayerId>,
    }

    impl Row {
        fn new(players: u8, board: Board) -> Self {
            Row {
                players,
                board,
                active: P0,
                optional: false,
                gate: Gate::None,
                otherwise: false,
                rider: Rider::None,
                tail: None,
                decline: None,
            }
        }
        fn optional(self) -> Self {
            Row {
                optional: true,
                ..self
            }
        }
        fn gate(self, gate: Gate) -> Self {
            Row { gate, ..self }
        }
        fn otherwise(self) -> Self {
            Row {
                otherwise: true,
                ..self
            }
        }
        fn rider(self, rider: Rider) -> Self {
            Row { rider, ..self }
        }
        fn tail(self, link: SubAbilityLink) -> Self {
            Row {
                tail: Some(link),
                ..self
            }
        }
        fn active(self, active: PlayerId) -> Self {
            Row { active, ..self }
        }
        fn decline(self, seat: PlayerId) -> Self {
            Row {
                decline: Some(seat),
                ..self
            }
        }
    }

    fn condition(gate: Gate) -> Option<AbilityCondition> {
        match gate {
            Gate::None => None,
            Gate::YourTurn => Some(AbilityCondition::IsYourTurn),
            Gate::ControlsEnchantment => Some(AbilityCondition::ControllerControlsMatching {
                filter: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
                ),
            }),
        }
    }

    /// `Token { Fractal, All }` → `PutCounter { LastCreated, All }` (with the
    /// row's optionality, condition and "otherwise" leg) → the row's rider →
    /// the row's trailing instruction.
    fn chain(source: ObjectId, row: &Row) -> ResolvedAbility {
        let mut token = ResolvedAbility::new(fractal_token(), vec![], source, P0);
        token.player_scope = Some(PlayerFilter::All);
        let mut place = placement(source);
        place.player_scope = Some(PlayerFilter::All);
        place.sub_link = SubAbilityLink::ContinuationStep;
        place.optional = row.optional;
        place.condition = condition(row.gate);
        if row.otherwise {
            place.else_ability = Some(Box::new(gain(2, source)));
        }
        let tail = row.tail.map(|link| {
            let mut tail = gain(3, source);
            tail.sub_link = link;
            Box::new(tail)
        });
        let performed = AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        };
        place.sub_ability = match row.rider {
            Rider::None => tail,
            rider => {
                let mut continuation = gain(1, source);
                match rider {
                    Rider::WhoDoesnt => {
                        continuation.condition = Some(AbilityCondition::Not {
                            condition: Box::new(performed),
                        });
                        continuation.sub_link = SubAbilityLink::SequentialSibling;
                    }
                    Rider::IfYouDo => {
                        continuation.condition = Some(performed);
                        continuation.sub_link = SubAbilityLink::SequentialSibling;
                    }
                    Rider::AndGain => continuation.sub_link = SubAbilityLink::ContinuationStep,
                    Rider::None => unreachable!(),
                }
                continuation.sub_ability = tail;
                Some(Box::new(continuation))
            }
        };
        token.sub_ability = Some(Box::new(place));
        token
    }

    /// The settled board of one row.
    #[derive(Debug)]
    struct Outcome {
        /// Each prompt answered, in order: `(seat, "optional" | "replacement")`.
        prompts: Vec<(PlayerId, &'static str)>,
        /// Each Fractal's `(owner, +1/+1 counters)`, sorted.
        fractals: Vec<(PlayerId, u32)>,
        /// Each player's life change.
        life: Vec<i32>,
    }

    fn fractals(runner: &GameRunner) -> Vec<(PlayerId, u32)> {
        let state = runner.state();
        let mut fractals: Vec<(PlayerId, u32)> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.name == "Fractal")
            .map(|object| {
                (
                    object.owner,
                    object
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                )
            })
            .collect();
        fractals.sort_by_key(|&(owner, counters)| (owner.0, counters));
        fractals
    }

    /// Answer every prompt until the resolution settles. Replacement order:
    /// Hardened Scales first when offered, otherwise the first non-halving
    /// candidate (Parallel Lives before Halving Season). `at_first_prompt` runs
    /// once, at the first prompt, before it is answered.
    fn drive(
        runner: &mut GameRunner,
        decline: Option<PlayerId>,
        at_first_prompt: &mut dyn FnMut(&mut GameRunner),
    ) -> Vec<(PlayerId, &'static str)> {
        let mut prompts = Vec::new();
        for _ in 0..100 {
            let at_prompt = !matches!(runner.state().waiting_for, WaitingFor::Priority { .. });
            if at_prompt && prompts.is_empty() {
                at_first_prompt(runner);
            }
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => return prompts,
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    prompts.push((player, "optional"));
                    runner
                        .act(GameAction::DecideOptionalEffect {
                            accept: decline != Some(player),
                        })
                        .expect("optional answer accepted");
                }
                WaitingFor::ReplacementChoice {
                    player, candidates, ..
                } => {
                    prompts.push((player, "replacement"));
                    let index = candidates
                        .iter()
                        .position(|candidate| candidate.description.contains("plus one"))
                        .or_else(|| {
                            candidates
                                .iter()
                                .position(|candidate| !candidate.description.contains("half"))
                        })
                        .unwrap_or(0);
                    runner
                        .act(GameAction::ChooseReplacement { index })
                        .expect("replacement choice accepted");
                }
                WaitingFor::UnlessPayment { player, .. } => {
                    prompts.push((player, "unless"));
                    runner
                        .act(GameAction::PayUnlessCost { pay: false })
                        .expect("unless answer accepted");
                }
                other => panic!("unexpected prompt {other:?}"),
            }
        }
        panic!("the resolution did not settle");
    }

    fn scenario(players: u8, board: Board) -> GameScenario {
        let mut scenario = if players == 2 {
            GameScenario::new()
        } else {
            GameScenario::new_n_player(players, 42)
        };
        scenario.at_phase(Phase::PreCombatMain);
        let mut add = |player: PlayerId, name: &str, text: &str| {
            scenario.add_enchantment_from_oracle(player, name, text);
        };
        match board {
            Board::Empty => {}
            Board::HalvingP0 => add(P0, "Halving Season", HALVING_SEASON),
            Board::HalvingP1 => add(P1, "Halving Season", HALVING_SEASON),
            Board::HalvingP2ParallelP0 => {
                add(P2, "Halving Season", HALVING_SEASON);
                add(P0, "Parallel Lives", PARALLEL_LIVES);
            }
            Board::ParallelP0 => add(P0, "Parallel Lives", PARALLEL_LIVES),
            Board::ScalesEvolutionP0 => {
                add(P0, "Hardened Scales", HARDENED_SCALES);
                add(P0, "Branching Evolution", BRANCHING_EVOLUTION);
            }
        }
        scenario
    }

    /// Resolve `chain` through the production resolver and settle it.
    fn settle(
        players: u8,
        board: Board,
        active: PlayerId,
        decline: Option<PlayerId>,
        chain: &dyn Fn(ObjectId) -> ResolvedAbility,
        at_first_prompt: &mut dyn FnMut(&mut GameRunner),
    ) -> Outcome {
        let mut runner = scenario(players, board).build();
        runner.state_mut().active_player = active;
        let source = create_object(
            runner.state_mut(),
            CardId(900),
            P0,
            "Constructed source".to_string(),
            Zone::Battlefield,
        );
        let before: Vec<i32> = runner.state().players.iter().map(|p| p.life).collect();
        resolve_ability_chain(runner.state_mut(), &chain(source), &mut Vec::new(), 0)
            .expect("chain resolves");
        let prompts = drive(&mut runner, decline, at_first_prompt);
        let life = runner
            .state()
            .players
            .iter()
            .zip(&before)
            .map(|(player, before)| player.life - before)
            .collect();
        Outcome {
            prompts,
            fractals: fractals(&runner),
            life,
        }
    }

    fn run(row: Row) -> Outcome {
        settle(
            row.players,
            row.board,
            row.active,
            row.decline,
            &|source| chain(source, &row),
            &mut |_| {},
        )
    }

    const LINKS: [SubAbilityLink; 2] = [
        SubAbilityLink::ContinuationStep,
        SubAbilityLink::SequentialSibling,
    ];

    // --- N5: the partition needs the matching co-scoped producer ------------

    /// An unscoped P0 `Token { Fractal }` followed by
    /// `PutCounter { LastCreated, scope All }`.
    fn unscoped_creation_then_each_player_placement(source: ObjectId) -> ResolvedAbility {
        let mut token = ResolvedAbility::new(fractal_token(), vec![], source, P0);
        let mut place = placement(source);
        place.player_scope = Some(PlayerFilter::All);
        place.sub_link = SubAbilityLink::ContinuationStep;
        token.sub_ability = Some(Box::new(place));
        token
    }

    /// N5-1 — the producer is not a co-scoped token clause, so both seats'
    /// instructions address the full latest-created set: P0's one Fractal.
    #[test]
    fn unscoped_creation_then_each_player_placement_reads_the_full_set() {
        let outcome = settle(
            2,
            Board::Empty,
            P0,
            None,
            &unscoped_creation_then_each_player_placement,
            &mut |_| {},
        );
        assert!(outcome.prompts.is_empty());
        assert_eq!(outcome.fractals, vec![(P0, 2)], "both seats' placements");
    }

    /// N5-2 — the same with a pause on P0's leg (CR 616.1: P0 controls the
    /// Fractal, so P0 orders both placements' replacements), and a whole-state
    /// serialization round trip at the first prompt.
    #[test]
    fn unscoped_creation_then_each_player_placement_survives_a_pause() {
        let mut first_prompt_fractals = None;
        let outcome = settle(
            2,
            Board::ScalesEvolutionP0,
            P0,
            None,
            &unscoped_creation_then_each_player_placement,
            &mut |runner| {
                first_prompt_fractals = Some(fractals(runner));
                let wire = serde_json::to_value(runner.state()).expect("state serializes");
                *runner.state_mut() = serde_json::from_value(wire).expect("state deserializes");
            },
        );
        assert_eq!(
            first_prompt_fractals,
            Some(vec![(P0, 0)]),
            "reach guard: the first prompt is P0's leg, before any placement"
        );
        assert_eq!(
            outcome.prompts,
            vec![(P0, "replacement"), (P0, "replacement")]
        );
        assert_eq!(outcome.fractals, vec![(P0, 8)], "(1 + 1) × 2, twice");
    }

    // --- B2: an empty seat keeps its continuation ---------------------------

    /// N5-3 — P0 Halving Season, so P1 creates no Fractal; P0 accepts its
    /// placement. P1 put no counter, so its "who doesn't" rider gives it 1 life.
    #[test]
    fn matched_seat_without_a_token_keeps_its_continuation_after_a_pause() {
        let outcome = run(Row::new(2, Board::HalvingP0)
            .optional()
            .rider(Rider::WhoDoesnt));
        assert_eq!(outcome.prompts, vec![(P0, "optional")]);
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![0, 1]);
    }

    /// N5-4 — synchronous, the empty seat first in APNAP order.
    #[test]
    fn matched_seat_without_a_token_keeps_its_continuation_first_in_apnap() {
        let outcome = run(Row::new(2, Board::HalvingP1).rider(Rider::WhoDoesnt));
        assert!(outcome.prompts.is_empty());
        assert_eq!(outcome.fractals, vec![(P1, 1)]);
        assert_eq!(outcome.life, vec![1, 0]);
    }

    /// N5-5 — synchronous, the empty seat second.
    #[test]
    fn matched_seat_without_a_token_keeps_its_continuation_last_in_apnap() {
        let outcome = run(Row::new(2, Board::HalvingP0).rider(Rider::WhoDoesnt));
        assert!(outcome.prompts.is_empty());
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![0, 1]);
    }

    /// N5-6 — three players, two empty seats below P0's accept latch.
    #[test]
    fn matched_seats_without_tokens_each_keep_their_continuation_after_a_pause() {
        let outcome = run(Row::new(3, Board::HalvingP0)
            .optional()
            .rider(Rider::WhoDoesnt));
        assert_eq!(outcome.prompts, vec![(P0, "optional")]);
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![0, 1, 1]);
    }

    /// N5-7 — the empty seat first, before P1's pause.
    #[test]
    fn matched_first_seat_without_a_token_keeps_its_continuation_before_a_pause() {
        let outcome = run(Row::new(2, Board::HalvingP1)
            .optional()
            .rider(Rider::WhoDoesnt));
        assert_eq!(outcome.prompts, vec![(P1, "optional")]);
        assert_eq!(outcome.fractals, vec![(P1, 1)]);
        assert_eq!(outcome.life, vec![1, 0]);
    }

    /// N5-8 — no empty seat: every seat placed, so no decline rider fires.
    #[test]
    fn matched_seats_that_place_do_not_take_the_decline_rider() {
        let outcome = run(Row::new(2, Board::Empty).rider(Rider::WhoDoesnt));
        assert_eq!(outcome.fractals, vec![(P0, 1), (P1, 1)]);
        assert_eq!(outcome.life, vec![0, 0]);
    }

    /// N5-9 — "If it's your turn, [you may] put a counter on it. Otherwise,
    /// gain 2 life. You gain 3 life." P1 (empty, not its turn) takes its
    /// "otherwise" leg, and the trailing instruction still runs once.
    #[test]
    fn matched_empty_seat_otherwise_leg_keeps_the_trailing_instruction_after_a_pause() {
        let outcome = run(Row::new(2, Board::HalvingP0)
            .optional()
            .gate(Gate::YourTurn)
            .otherwise()
            .tail(SubAbilityLink::SequentialSibling));
        assert_eq!(outcome.prompts, vec![(P0, "optional")]);
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![3, 2]);
    }

    #[test]
    fn matched_empty_seat_otherwise_leg_keeps_the_trailing_instruction_in_apnap() {
        let outcome = run(Row::new(2, Board::HalvingP0)
            .gate(Gate::YourTurn)
            .otherwise()
            .tail(SubAbilityLink::SequentialSibling));
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![3, 2]);
    }

    /// N5-10 — two consecutive "otherwise" legs, each carrying what follows.
    #[test]
    fn matched_empty_seats_otherwise_legs_each_run_after_a_pause() {
        let row = Row::new(3, Board::HalvingP0)
            .gate(Gate::YourTurn)
            .otherwise()
            .tail(SubAbilityLink::SequentialSibling);
        let paused = run(row.optional());
        assert_eq!(paused.prompts, vec![(P0, "optional")]);
        assert_eq!(paused.life, vec![3, 2, 2]);
        let sync = run(row);
        assert_eq!(sync.life, vec![3, 2, 2]);
    }

    /// N5-11 — a failed condition with "each player who doesn't": the empty
    /// seat reads its own "not performed", not P0's accept latch.
    #[test]
    fn matched_empty_seat_failed_condition_reads_its_own_outcome_after_a_pause() {
        for link in LINKS {
            for (players, expected) in [(2, vec![3, 1]), (3, vec![3, 1, 1])] {
                let row = Row::new(players, Board::HalvingP0)
                    .gate(Gate::YourTurn)
                    .rider(Rider::WhoDoesnt)
                    .tail(link);
                let paused = run(row.optional());
                assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
                assert_eq!(paused.life, expected, "paused, {players}p, {link:?}");
                assert_eq!(run(row).life, expected, "sync, {players}p, {link:?}");
            }
        }
    }

    /// N5-12 — a failed condition governs the whole sentence, so the empty
    /// seat's dependent "and gain 1 life" is skipped; the trailing instruction
    /// still runs, whatever its link.
    #[test]
    fn matched_empty_seat_failed_condition_skips_its_dependent_continuation() {
        for link in LINKS {
            let row = Row::new(2, Board::HalvingP0)
                .optional()
                .gate(Gate::YourTurn)
                .rider(Rider::AndGain)
                .tail(link);
            let paused = run(row);
            assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
            assert_eq!(paused.life, vec![4, 0], "paused, 2p, {link:?}");
            let paused = run(Row { players: 3, ..row });
            assert_eq!(paused.life, vec![4, 0, 0], "paused, 3p, {link:?}");
            // Sync controls: on P1's (P2's) turn P0's condition fails, so no
            // seat is prompted; the empty seat whose condition holds is not
            // asserted (suppress-to-no-op limitation).
            let sync = run(row.active(P1));
            assert!(sync.prompts.is_empty());
            assert_eq!(sync.life[0], 3, "sync, 2p, {link:?}");
            let sync = run(Row { players: 3, ..row }.active(P2));
            assert!(sync.prompts.is_empty());
            assert_eq!(sync.life[..2], [3, 0], "sync, 3p, {link:?}");
        }
    }

    /// N5-14 — a failed condition with "if you do": the empty seat gains
    /// nothing, P0 (accepted) gains 1, and the trailing instruction runs.
    #[test]
    fn matched_empty_seat_failed_condition_false_if_you_do_keeps_the_trailing_instruction() {
        for link in LINKS {
            for (players, expected) in [(2, vec![4, 0]), (3, vec![4, 0, 0])] {
                let row = Row::new(players, Board::HalvingP0)
                    .gate(Gate::YourTurn)
                    .rider(Rider::IfYouDo)
                    .tail(link);
                let paused = run(row.optional());
                assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
                assert_eq!(paused.fractals, vec![(P0, 1)]);
                assert_eq!(paused.life, expected, "paused, {players}p, {link:?}");
                assert_eq!(run(row).life, expected, "sync, {players}p, {link:?}");
            }
        }
    }

    /// N5-15 — the unconditioned B2 shape: the empty seat's "if you do" is
    /// false, and the trailing instruction runs.
    #[test]
    fn matched_empty_seat_false_if_you_do_keeps_the_trailing_instruction() {
        for link in LINKS {
            for (players, board, prompted, expected) in [
                (2, Board::HalvingP0, P0, vec![4, 0]),
                (3, Board::HalvingP0, P0, vec![4, 0, 0]),
                (2, Board::HalvingP1, P1, vec![3, 1]),
            ] {
                let row = Row::new(players, board).rider(Rider::IfYouDo).tail(link);
                let paused = run(row.optional());
                assert_eq!(paused.prompts, vec![(prompted, "optional")], "{link:?}");
                assert_eq!(paused.fractals, vec![(prompted, 1)]);
                assert_eq!(paused.life, expected, "paused, {players}p, {link:?}");
                assert_eq!(run(row).life, expected, "sync, {players}p, {link:?}");
            }
        }
    }

    /// N5-16 — every seat `Bound`; the last seat does not place (its condition
    /// fails, or it declines its "may"). The trailing instruction runs once.
    #[test]
    fn bound_last_seat_that_does_not_place_keeps_the_trailing_instruction() {
        for link in LINKS {
            // (i) P0 Parallel Lives (P0 2 Fractals, P1 1); P1 controls no
            // enchantment, so its condition fails. P1's own life is not asserted.
            let row = Row::new(2, Board::ParallelP0)
                .gate(Gate::ControlsEnchantment)
                .rider(Rider::WhoDoesnt)
                .tail(link);
            let paused = run(row.optional());
            assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
            assert_eq!(paused.fractals, vec![(P0, 1), (P0, 1), (P1, 0)]);
            assert_eq!(paused.life[0], 3, "paused (i), {link:?}");
            assert_eq!(run(row).life[0], 3, "sync (i), {link:?}");

            // (ii) unconditioned; P0 accepts, P1 declines.
            let row = Row::new(2, Board::Empty)
                .optional()
                .rider(Rider::IfYouDo)
                .tail(link);
            let declined = run(row.decline(P1));
            assert_eq!(
                declined.prompts,
                vec![(P0, "optional"), (P1, "optional")],
                "{link:?}"
            );
            assert_eq!(declined.life, vec![4, 0], "(ii), {link:?}");

            // (iii) three players; P1, then P2, declines.
            let row = Row { players: 3, ..row };
            assert_eq!(
                run(row.decline(P1)).life,
                vec![4, 0, 0],
                "(iii) P1, {link:?}"
            );
            assert_eq!(
                run(row.decline(P2)).life,
                vec![4, 1, 0],
                "(iii) P2, {link:?}"
            );
        }
    }

    /// L1 — CR 608.2c + CR 118.12a: "If it's your turn, put a counter on it
    /// unless [you] pay {1}. Each player who doesn't gains 1 life." The empty
    /// seat (P1, Halving Season) offers its unless payment once, and only when
    /// its condition holds: the condition is checked before the payment choice.
    #[test]
    fn matched_empty_seat_offers_its_unless_payment_once_behind_its_condition() {
        for (active, expected) in [(P1, vec![(P1, "unless")]), (P0, vec![(P0, "unless")])] {
            let row = Row::new(2, Board::HalvingP1)
                .gate(Gate::YourTurn)
                .rider(Rider::WhoDoesnt)
                .active(active);
            let outcome = settle(
                2,
                Board::HalvingP1,
                active,
                None,
                &|source| {
                    let mut chain = chain(source, &row);
                    chain
                        .sub_ability
                        .as_deref_mut()
                        .expect("the placement follows the creation")
                        .unless_pay = Some(UnlessPayModifier {
                        cost: AbilityCost::Mana {
                            cost: ManaCost::generic(1),
                        },
                        payer: TargetFilter::Controller,
                    });
                    chain
                },
                &mut |_| {},
            );
            assert_eq!(outcome.prompts, expected, "{active:?}'s turn");
        }
    }

    /// N5-13 — the empty seat's failed condition and false "if you do" before a
    /// placing seat: P2's leg is reached with its own ability.
    #[test]
    fn matched_empty_seat_before_a_placing_seat_after_a_pause() {
        for link in LINKS {
            let row = Row::new(3, Board::HalvingP2ParallelP0)
                .gate(Gate::ControlsEnchantment)
                .rider(Rider::IfYouDo)
                .tail(link);
            let paused = run(row.optional());
            assert_eq!(
                paused.prompts,
                vec![(P0, "replacement"), (P0, "optional"), (P2, "optional")],
                "{link:?}"
            );
            // P0 keeps one Fractal (Parallel Lives, then P2's Halving Season);
            // P0's counter is halved to 0 by P2's Halving Season (P0 is P2's
            // opponent); P1 created none; P2's own placement lands.
            assert_eq!(paused.fractals, vec![(P0, 0), (P2, 1)]);
            assert_eq!(paused.life[1..], [0, 1], "paused, {link:?}");
            let sync = run(row);
            assert!(sync.fractals.contains(&(P2, 1)));
            assert_eq!(sync.life[1..], [0, 1], "sync, {link:?}");
        }
    }
}
