//! Printed-card witnesses for conditional mana self-replacement.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const TOWER: &str = "Urza's Tower";
const MINE: &str = "Urza's Mine";
const PLANT: &str = "Urza's Power Plant";
const TRON: [&str; 3] = [TOWER, MINE, PLANT];

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn add(scenario: &mut GameScenario, name: &str, zone: Zone) -> ObjectId {
    scenario.add_real_card(
        P0,
        name,
        zone,
        shared_card_db().expect("printed card fixture"),
    )
}

fn add_tron(scenario: &mut GameScenario) -> [ObjectId; 3] {
    TRON.map(|name| add(scenario, name, Zone::Battlefield))
}

fn play_land(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::PlayLand { object_id, card_id })
        .unwrap();
    assert_eq!(runner.state().objects[&object_id].zone, Zone::Battlefield);
}

fn mana_tap_payloads(events: &[GameEvent], source: ObjectId) -> Vec<&[ManaType]> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::TappedForMana {
                source_id,
                produced,
                ..
            } if *source_id == source => Some(produced.as_slice()),
            _ => None,
        })
        .collect()
}

fn assert_production(runner: &mut GameRunner, source: ObjectId, mana: ManaType, amount: usize) {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // CR 605.3b + CR 614.15: the activation immediately produces the selected amount.
    let name = &runner.state().objects[&source].name;
    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(pool.count_color(mana), amount, "{name}");
    assert_eq!(pool.total(), amount, "{name}");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    // CR 106.12a: one resolution contributes one tap event with its full output.
    assert_eq!(
        mana_tap_payloads(&result.events, source),
        [vec![mana; amount].as_slice()],
        "{name}"
    );
}

#[test]
fn urza_lands_produce_base_or_full_replacement() {
    for (source_index, name) in TRON.into_iter().enumerate() {
        for assembled in [false, true] {
            let mut scenario = main_phase();
            let source = if assembled {
                add_tron(&mut scenario)[source_index]
            } else {
                add(&mut scenario, name, Zone::Battlefield)
            };
            let amount = if !assembled {
                1
            } else if name == TOWER {
                3
            } else {
                2
            };
            assert_production(&mut scenario.build(), source, ManaType::Colorless, amount);
        }
    }
}

#[test]
fn tower_worker_requires_both_named_workers() {
    for (companions, amount) in [
        (&[][..], 1),
        (&["Mine Worker"][..], 1),
        (&["Power Plant Worker"][..], 1),
        (&["Mine Worker", "Power Plant Worker"][..], 3),
    ] {
        let mut scenario = main_phase();
        let tower = add(&mut scenario, "Tower Worker", Zone::Battlefield);
        for name in companions {
            add(&mut scenario, name, Zone::Battlefield);
        }
        assert_production(&mut scenario.build(), tower, ManaType::Colorless, amount);
    }
}

#[test]
fn tower_worker_reads_its_companions_in_the_order_auto_payment_activates_sources() {
    let mut scenario = main_phase();
    let tower = add(&mut scenario, "Tower Worker", Zone::Battlefield);
    let mine = add(&mut scenario, "Mine Worker", Zone::Battlefield);
    add(&mut scenario, "Power Plant Worker", Zone::Battlefield);
    let cauldron = add(&mut scenario, "Agatha's Soul Cauldron", Zone::Battlefield);
    let cantor = add(&mut scenario, "Wild Cantor", Zone::Graveyard);
    let spell = add(&mut scenario, "Explore", Zone::Hand);
    let mut runner = scenario.build();

    // CR 603.12 + CR 113.2c: the Cauldron exiles Wild Cantor and puts its counter on Mine Worker.
    let grant = runner
        .activate(cauldron, 0)
        .target_objects(&[cantor, mine])
        .resolve();
    grant.assert_zone(&[cantor], Zone::Exile);
    grant.assert_counters(mine, CounterType::Plus1Plus1, 1);
    assert!(matches!(
        grant.final_waiting_for(),
        WaitingFor::Priority { player: P0 }
    ));

    let card_id = runner.state().objects[&spell].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("automatic payment can attempt to use the two available sources");
    let sacrificed = result
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::PermanentSacrificed { object_id, player_id: P0 } if *object_id == mine
            )
        })
        .expect("Mine Worker supplies green through Wild Cantor's granted ability");
    let tower_tapped = result
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::TappedForMana { source_id, .. } if *source_id == tower
            )
        })
        .expect("Tower Worker supplies colorless");

    // CR 605.3b + CR 614.15: the replacement is chosen against the board at the actual tap.
    assert!(
        sacrificed < tower_tapped,
        "Mine Worker must leave before Tower Worker taps"
    );
    assert_eq!(
        mana_tap_payloads(&result.events, tower),
        [&[ManaType::Colorless][..]]
    );
    // CR 601.2g + CR 601.2h: Explore spends the granted green and one colorless.
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn labyrinth_produces_base_or_full_replacement_after_imprint_choice() {
    for imprint in [false, true] {
        let mut scenario = main_phase();
        let land = add(&mut scenario, "Ugin's Labyrinth", Zone::Hand);
        let candidate = add(&mut scenario, "Kozilek, Butcher of Truth", Zone::Hand);
        let other = add(&mut scenario, "Ulamog, the Infinite Gyre", Zone::Hand);
        let mut runner = scenario.build();
        play_land(&mut runner, land);
        assert_eq!(runner.state().stack.len(), 1, "Imprint is on the stack");
        runner.pass_both_players();
        // CR 406.6 + CR 607.2a: the printed Imprint ability establishes the link.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: imprint })
            .unwrap();
        if imprint {
            let WaitingFor::EffectZoneChoice {
                player: P0,
                ref cards,
                ..
            } = runner.state().waiting_for
            else {
                panic!(
                    "expected Imprint hand choice, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(cards.len(), 2);
            assert!(cards.contains(&candidate) && cards.contains(&other));
            runner
                .act(GameAction::SelectCards {
                    cards: vec![candidate],
                })
                .unwrap();
        }
        assert_eq!(runner.state().objects[&other].zone, Zone::Hand);
        assert_eq!(
            runner.state().objects[&candidate].zone,
            if imprint { Zone::Exile } else { Zone::Hand }
        );
        assert_eq!(
            runner
                .state()
                .exile_links
                .iter()
                .any(|link| link.source_id == land && link.exiled_id == candidate),
            imprint
        );
        assert_production(
            &mut runner,
            land,
            ManaType::Colorless,
            if imprint { 2 } else { 1 },
        );
    }
}

#[test]
fn tome_produces_base_or_full_replacement_with_the_initiative() {
    for take_initiative in [false, true] {
        let mut scenario = main_phase();
        let tome = add(
            &mut scenario,
            "Sarevok's Tome",
            if take_initiative {
                Zone::Hand
            } else {
                Zone::Battlefield
            },
        );
        if take_initiative {
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 4],
            );
        }
        let mut runner = scenario.build();
        if take_initiative {
            let outcome = runner.cast(tome).search_none().resolve();
            assert_eq!(outcome.zone_of(tome), Zone::Battlefield);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { player: P0 }
            ));
        }
        // CR 726.1: the Tome's ETB grants the initiative only in the cast case.
        assert_eq!(runner.state().initiative, take_initiative.then_some(P0));
        assert_production(
            &mut runner,
            tome,
            ManaType::Colorless,
            if take_initiative { 2 } else { 1 },
        );
    }
}

#[test]
fn tower_reads_the_board_at_activation_after_playing_power_plant() {
    let mut scenario = main_phase();
    let tower = add(&mut scenario, TOWER, Zone::Battlefield);
    add(&mut scenario, MINE, Zone::Battlefield);
    let plant = add(&mut scenario, PLANT, Zone::Hand);
    let mut runner = scenario.build();
    let mut without_plant = GameRunner::from_state(runner.state().clone());
    assert_production(&mut without_plant, tower, ManaType::Colorless, 1);
    // CR 305.1 + CR 614.15: playing the Plant before activation changes the selected production.
    play_land(&mut runner, plant);
    assert_production(&mut runner, tower, ManaType::Colorless, 3);
}

#[test]
fn wild_growth_observes_one_tower_tap_with_full_production() {
    let mut scenario = main_phase();
    let [tower, ..] = add_tron(&mut scenario);
    let growth = add(&mut scenario, "Wild Growth", Zone::Hand);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let aura = runner.cast(growth).target_object(tower).resolve();
    assert_eq!(aura.zone_of(growth), Zone::Battlefield);

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: tower,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // CR 106.12a + CR 605.4a: the whole tap production triggers Wild Growth once.
    assert_eq!(
        mana_tap_payloads(&result.events, tower),
        [&[ManaType::Colorless; 3][..]]
    );
    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(pool.count_color(ManaType::Colorless), 3);
    assert_eq!(pool.count_color(ManaType::Green), 1);
    assert_eq!(result.events.iter().filter(|event| matches!(event,
        GameEvent::ManaAdded { source_id, mana_type: ManaType::Green, .. } if *source_id == growth
    )).count(), 1);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn mana_reflection_doubles_the_entire_tower_replacement() {
    let mut scenario = main_phase();
    let [tower, ..] = add_tron(&mut scenario);
    add(&mut scenario, "Mana Reflection", Zone::Battlefield);
    // CR 106.12b + CR 614.15 + CR 616.1a: select three, then double the selected production.
    let mut runner = scenario.build();
    let outcome = runner.activate(tower, 0).resolve();
    assert_eq!(outcome.mana_pool_color(P0, ManaType::Colorless), 6);
    assert_eq!(outcome.mana_pool_total(P0), 6);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn river_of_tears_and_reflecting_pool_see_the_same_land_play() {
    for land_played in [false, true] {
        let mut scenario = main_phase();
        let river = add(&mut scenario, "River of Tears", Zone::Battlefield);
        let pool = add(&mut scenario, "Reflecting Pool", Zone::Battlefield);
        let other_pool = add(&mut scenario, "Reflecting Pool", Zone::Hand);
        let mut runner = scenario.build();
        if land_played {
            // Playing the other Reflecting Pool leaves River as the only source
            // of a type, including on the land-played branch.
            play_land(&mut runner, other_pool);
        }
        let mana = if land_played {
            ManaType::Black
        } else {
            ManaType::Blue
        };
        let mut reading = GameRunner::from_state(runner.state().clone());
        // CR 106.7: Reflecting Pool sees River's current output type.
        assert_production(&mut reading, pool, mana, 1);
        assert_production(&mut runner, river, mana, 1);
    }
}

#[test]
fn river_of_tears_auto_payment_uses_the_selected_color() {
    for (land_played, spell_name, mana) in [
        (false, "Consider", ManaType::Blue),
        (true, "Dark Ritual", ManaType::Black),
    ] {
        let mut scenario = main_phase();
        let river = add(&mut scenario, "River of Tears", Zone::Battlefield);
        let land = add(&mut scenario, "Reflecting Pool", Zone::Hand);
        let spell = add(&mut scenario, spell_name, Zone::Hand);
        let mut runner = scenario.build();
        if land_played {
            play_land(&mut runner, land);
            runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
        }

        let card_id = runner.state().objects[&spell].card_id;
        let result = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert_eq!(mana_tap_payloads(&result.events, river), [&[mana][..]]);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    }
}

#[test]
fn ballista_offers_and_pays_x_one_without_tron_and_x_three_with_tron() {
    for (land_names, x, leftover) in [(&TRON[..2], 1, 0), (&TRON[..], 3, 1)] {
        let mut scenario = main_phase();
        let lands: Vec<_> = land_names
            .iter()
            .map(|name| add(&mut scenario, name, Zone::Battlefield))
            .collect();
        let ballista = add(&mut scenario, "Walking Ballista", Zone::Hand);
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&ballista].card_id;
        let announced = runner
            .act(GameAction::CastSpell {
                object_id: ballista,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        // CR 107.3a + CR 601.2g + CR 601.2h: a double-X spell costs two per X.
        // These boards can supply 2 or 7 mana respectively.
        assert!(
            matches!(
                announced.waiting_for,
                WaitingFor::ChooseXValue { player: P0, max, .. } if max == x
            ),
            "expected maximum X={x} with {land_names:?}, got {:?}",
            announced.waiting_for
        );
        let paid = runner.act(GameAction::ChooseX { value: x }).unwrap();
        assert!(matches!(
            paid.waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert_eq!(runner.state().stack.len(), 1);
        assert!(lands
            .into_iter()
            .all(|land| runner.state().objects[&land].tapped));
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Colorless),
            leftover
        );
        runner.advance_until_stack_empty();
        let permanent = &runner.state().objects[&ballista];
        assert_eq!(permanent.zone, Zone::Battlefield);
        // CR 107.3m: the Ballista enters using the X announced for its spell.
        assert_eq!(permanent.counters.get(&CounterType::Plus1Plus1), Some(&x));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
    }
}

#[test]
fn tron_auto_taps_to_cast_a_four_mana_spell() {
    let mut scenario = main_phase();
    let lands = add_tron(&mut scenario);
    let spell = add(&mut scenario, "Juggernaut", Zone::Hand);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    // CR 601.2g + CR 601.2h: automatic mana activation pays the full {4}.
    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { player: P0 }
    ));
    let mut produced = 0;
    let mut activated = 0;
    for (land, amount) in lands.into_iter().zip([3, 2, 2]) {
        let taps = mana_tap_payloads(outcome.events(), land);
        if outcome.state().objects[&land].tapped {
            assert_eq!(taps, [&[ManaType::Colorless; 3][..amount]]);
            produced += amount;
            activated += 1;
        } else {
            assert!(taps.is_empty());
        }
    }
    // The planner's generic scan stops when it covers the remaining cost.
    // Any two of the assembled lands cover it.
    assert_eq!(activated, 2);
    assert_eq!(
        produced,
        4 + outcome.mana_pool_color(P0, ManaType::Colorless) as usize
    );
}
