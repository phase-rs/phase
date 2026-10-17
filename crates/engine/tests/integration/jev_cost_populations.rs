//! Coordinated card populations through the production casting pipeline.
use super::support::shared_card_export_json;
use engine::database::card_db::CardDatabase;
use engine::game::game_object::BackFaceData;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, ContinuousModification, ControllerRef, Duration, Effect, FilterProp, QuantityExpr,
    QuantityRef, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card::LayoutKind;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;
use serde_json::{to_string, Map, Value};
use std::path::Path;

const SAILORS: &str = "This spell costs {1} less to cast for each card you own in exile and in your graveyard that's an instant card, a sorcery card, or a card that has an Adventure.\nWard {4} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {4}.)";

fn blue_cost(generic: u32, blue: usize) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::Blue; blue],
    }
}

fn mana(scenario: &mut GameScenario, count: usize) {
    scenario.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn sailors_board(populated: bool, pool: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, pool);
    let spell = scenario
        .add_creature_to_hand(P0, "Sailors' Bane", 7, 7)
        .with_mana_cost(blue_cost(7, 2))
        .from_oracle_text_with_keywords(&["Ward"], SAILORS)
        .id();
    let mut adventure_ids = Vec::new();
    if populated {
        let overlapping = scenario
            .add_spell_to_exile(P0, "Synthetic instant with Adventure", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic sorcery", false);
        let adventure = scenario
            .add_creature_to_graveyard(P0, "Synthetic Adventure permanent", 1, 1)
            .id();
        // CR 715.2a + CR 715.2c: actual alternative-face metadata qualifies a
        // permanent; the instant with the same metadata still counts only once.
        adventure_ids.extend([overlapping, adventure]);
        scenario.add_spell_to_exile(P1, "Wrong owner exile", true);
        scenario.add_spell_to_graveyard(P1, "Opponent graveyard", false);
        scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
        scenario.add_spell_to_hand(P0, "Wrong zone instant", true);
    }
    let mut runner = scenario.build();
    for id in adventure_ids {
        runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
            name: "Synthetic Adventure alternative".into(),
            layout_kind: Some(LayoutKind::Adventure),
            ..Default::default()
        });
    }
    (runner, spell)
}

#[test]
fn sailors_population_cost() {
    for (populated, pool) in [(true, 6), (false, 9)] {
        let (mut runner, spell) = sailors_board(populated, pool);
        assert_eq!(runner.state().objects[&spell].static_definitions.len(), 1);
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        // CR 118.7a + CR 601.2f: three qualifying cards remove three generic
        // mana, retaining both blue pips; all hostile cards contribute zero.
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    let (mut runner, spell) = sailors_board(true, 6);
    runner.state_mut().players[0].mana_pool = (0..6)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    // CR 118.7a: the generic reduction cannot pay either blue pip.
    assert!(runner.cast(spell).try_resolve().is_err());
    let (mut runner, spell) = sailors_board(true, 5);
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "one short cannot pay the locked cost"
    );
}

const CANDLEKEEP: &str = "Until end of turn, creatures you control have base power and toughness X/X, where X is the number of cards you own in exile and in your graveyard that are instant cards, are sorcery cards, and/or have an Adventure.";

fn candlekeep_board(
    populated: bool,
    recipients: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let spell = scenario
        .add_spell_to_hand(P0, "Candlekeep Inspiration", false)
        .with_mana_cost(blue_cost(4, 1))
        .from_oracle_text(CANDLEKEEP)
        .id();
    let mut controlled = Vec::new();
    if recipients {
        controlled.push(scenario.add_creature(P0, "Own recipient", 6, 7).id());
        controlled.push(
            scenario
                .add_creature(P0, "Foreign-owned recipient", 4, 5)
                .id(),
        );
    }
    let opponent = scenario.add_creature(P1, "Opponent recipient", 8, 9).id();
    let own_opponent = scenario
        .add_creature(P1, "Own opponent-controlled", 10, 11)
        .id();
    let late = scenario.add_creature_to_hand(P0, "Late entrant", 5, 6).id();
    let mut adventure_ids = Vec::new();
    if populated {
        let overlap = scenario
            .add_spell_to_exile(P0, "Instant Adventure", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Sorcery member", false);
        let adventure = scenario
            .add_creature_to_graveyard(P0, "Adventure permanent", 1, 1)
            .id();
        adventure_ids.extend([overlap, adventure]);
        scenario.add_spell_to_exile(P1, "Foreign exile", true);
        scenario.add_spell_to_graveyard(P1, "Opponent graveyard", false);
        scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
        scenario.add_spell_to_hand(P0, "Hand instant", true);
    }
    let mut runner = scenario.build();
    for id in adventure_ids {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.controller = P1;
        object.back_face = Some(BackFaceData {
            name: "Adventure alternative".into(),
            layout_kind: Some(LayoutKind::Adventure),
            ..Default::default()
        });
    }
    if recipients {
        runner
            .state_mut()
            .objects
            .get_mut(&controlled[1])
            .unwrap()
            .owner = P1;
    }
    runner
        .state_mut()
        .objects
        .get_mut(&own_opponent)
        .unwrap()
        .owner = P0;
    (runner, spell, controlled, opponent, late)
}

#[test]
fn candlekeep_population_base_pt() {
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(true, true);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    // CR 608.2h + CR 613.4b: the complete three-member population sets both
    // base characteristics when this sorcery resolves, before it enters the graveyard.
    for id in controlled {
        let obj = &outcome.state().objects[&id];
        assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
    }
    let obj = &outcome.state().objects[&opponent];
    assert_eq!((obj.power, obj.toughness), (Some(8), Some(9)));
    let other = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Own opponent-controlled")
        .unwrap();
    assert_eq!((other.power, other.toughness), (Some(10), Some(11)));
}

#[test]
fn candlekeep_population_resolution_binding() {
    let (mut runner, spell, controlled, _, late) = candlekeep_board(true, true);
    runner
        .cast(spell)
        .resolve()
        .assert_zone(&[spell], Zone::Graveyard);
    let assert_three = |runner: &GameRunner| {
        for id in &controlled {
            let obj = &runner.state().objects[id];
            assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
        }
    };
    // CR 611.2d: the source becoming a fourth graveyard member cannot update X.
    assert_three(&runner);
    let hand_instant = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Hand instant")
        .unwrap()
        .id;
    engine::game::zones::remove_from_zone(runner.state_mut(), hand_instant, Zone::Hand, P0);
    runner
        .state_mut()
        .objects
        .get_mut(&hand_instant)
        .unwrap()
        .zone = Zone::Graveyard;
    engine::game::zones::add_to_zone(runner.state_mut(), hand_instant, Zone::Graveyard, P0);
    engine::game::zones::remove_from_zone(runner.state_mut(), late, Zone::Hand, P0);
    runner.state_mut().objects.get_mut(&late).unwrap().zone = Zone::Battlefield;
    engine::game::zones::add_to_zone(runner.state_mut(), late, Zone::Battlefield, P0);
    {
        let recipient = runner.state_mut().objects.get_mut(&controlled[0]).unwrap();
        recipient.controller = P1;
        recipient.base_controller = Some(P1);
    }
    engine::game::layers::evaluate_layers(runner.state_mut());
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    // CR 611.2c: resolution fixes the recipients even after entry/control changes.
    assert_three(&runner);
    assert_eq!(runner.state().objects[&controlled[0]].controller, P1);
    let obj = &runner.state().objects[&late];
    assert_eq!((obj.power, obj.toughness), (Some(5), Some(6)));
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    // CR 514.2: real cleanup expires the effect and restores printed values.
    let obj = &runner.state().objects[&controlled[0]];
    assert_eq!((obj.power, obj.toughness), (Some(6), Some(7)));
    let obj = &runner.state().objects[&controlled[1]];
    assert_eq!((obj.power, obj.toughness), (Some(4), Some(5)));
}

#[test]
fn candlekeep_population_empty_cases() {
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(false, true);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    // CR 704.5f: zero base toughness is followed by the ordinary state-based death.
    outcome.assert_zone(&controlled, Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&opponent].power, Some(8));
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(true, false);
    assert!(controlled.is_empty());
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&opponent].toughness, Some(9));
}

#[test]
fn coordinated_population_draw_composition() {
    // Synthetic Sorcery witnesses: these compose the repaired population with
    // existing Draw/Offset/Multiply consumers, without replacing printed Oracle.
    for (introduction, populated_draws, empty_draws) in [
        ("the number of ", 3, 0),
        ("the total number of ", 3, 0),
        ("one plus the total number of ", 4, 1),
        ("twice the number of ", 6, 0),
    ] {
        for (populated, expected) in [(true, populated_draws), (false, empty_draws)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            mana(&mut scenario, 2);
            let source = format!("Draw X cards, where X is {introduction}cards you own in exile and in your graveyard that are instant cards, are sorcery cards, and/or have an Adventure.");
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic population Draw", false)
                .with_mana_cost(blue_cost(1, 1))
                .from_oracle_text(&source)
                .id();
            // Known qualifying types in Hand and Library must contribute zero.
            scenario.add_spell_to_hand(P0, "Hand instant", true);
            scenario.add_spell_to_hand(P1, "Opponent hand sorcery", false);
            for player in [P0, P1] {
                for index in 0..8 {
                    scenario.add_spell_to_library_top(
                        player,
                        &format!("Known library instant {index}"),
                        true,
                    );
                }
            }
            let mut adventure_ids = Vec::new();
            let mut foreign_ids = Vec::new();
            if populated {
                let overlap = scenario
                    .add_spell_to_exile(P0, "Instant Adventure member", true)
                    .id();
                let sorcery = scenario
                    .add_spell_to_graveyard(P0, "Sorcery member", false)
                    .id();
                let adventure = scenario
                    .add_creature_to_graveyard(P0, "Adventure permanent member", 1, 1)
                    .id();
                adventure_ids.extend([overlap, adventure]);
                foreign_ids.push(
                    scenario
                        .add_spell_to_exile(P1, "Foreign exile instant", true)
                        .id(),
                );
                foreign_ids.push(
                    scenario
                        .add_spell_to_graveyard(P1, "Opponent graveyard sorcery", false)
                        .id(),
                );
                scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
                let mut runner = scenario.build();
                // CR 108.3 + CR 109.4: off-zone controller values cannot replace
                // ownership; hostile foreign cards coexist with positive members.
                for id in [overlap, sorcery, adventure] {
                    runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
                }
                for id in foreign_ids {
                    runner.state_mut().objects.get_mut(&id).unwrap().controller = P0;
                }
                // CR 715.2a + CR 715.2c: actual Adventure metadata qualifies the
                // permanent; Instant plus Adventure is still one card.
                for id in adventure_ids {
                    runner.state_mut().objects.get_mut(&id).unwrap().back_face =
                        Some(BackFaceData {
                            name: "Adventure alternative".into(),
                            layout_kind: Some(LayoutKind::Adventure),
                            ..Default::default()
                        });
                }
                assert_population_draw(&mut runner, spell, expected);
            } else {
                assert_population_draw(&mut scenario.build(), spell, expected);
            }
        }
    }
}

fn assert_population_draw(runner: &mut GameRunner, spell: ObjectId, expected: i64) {
    let outcome = runner.cast(spell).resolve();
    // CR 121.1 + CR 121.2: measure actual library-to-hand deliveries from stack
    // commit. CR 608.2h: the source is on Stack at the one population read;
    // its subsequent graveyard arrival cannot add another draw.
    outcome.assert_hand_drawn(P0, expected);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn coordinated_population_export_string_loader() {
    const BEACON: &str = "Beacon Bolt deals damage to target creature equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nJump-start (You may cast this card from your graveyard by discarding a card in addition to paying its other costs. Then exile this card.)";
    const DRAKE: &str = "Flying\nCrackling Drake's power is equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nWhen this creature enters, draw a card.";
    const HUSKBURSTER: &str = "This spell costs {1} less to cast for each creature card you own in exile and in your graveyard.\nMenace, deathtouch";
    const RAL: &str = "[+1]: Look at the top two cards of your library. Put one of them into your hand and the other into your graveyard.\n[−3]: Ral deals damage to target creature equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\n[−8]: You get an emblem with \"Whenever you cast an instant or sorcery spell, this emblem deals 4 damage to any target and you draw two cards.\"";
    const CURVE: &str = "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is one plus the total number of instant and sorcery cards you own in exile and in your graveyard.";

    let client_export =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    assert!(
        client_export.exists(),
        "generated client export is required"
    );
    let raw = shared_card_export_json().expect(
        "generated full export is required for coordinated population string-loader regression",
    );
    let mut entries = Map::<String, Value>::new();
    for (key, name, oracle) in [
        ("sailors' bane", "Sailors' Bane", SAILORS),
        (
            "candlekeep inspiration",
            "Candlekeep Inspiration",
            CANDLEKEEP,
        ),
        ("beacon bolt", "Beacon Bolt", BEACON),
        ("crackling drake", "Crackling Drake", DRAKE),
        ("huskburster swarm", "Huskburster Swarm", HUSKBURSTER),
        ("ral, izzet viceroy", "Ral, Izzet Viceroy", RAL),
        ("serpentine curve", "Serpentine Curve", CURVE),
    ] {
        let entry = raw.get(key).expect("required changed face in full export");
        assert_eq!(entry.get("name").and_then(Value::as_str), Some(name));
        assert_eq!(
            entry.get("oracle_text").and_then(Value::as_str),
            Some(oracle)
        );
        entries.insert(key.into(), entry.clone());
    }
    assert_eq!(entries.len(), 7);
    let serialized = to_string(&entries).expect("unchanged raw entries serialize");
    let db = CardDatabase::from_json_str(&serialized).expect("real changed export entries load");
    assert_eq!(db.face_iter().count(), 7);
    let sailors = db
        .get_face_by_name("Sailors' Bane")
        .expect("loaded Sailors' Bane");
    let candlekeep = db
        .get_face_by_name("Candlekeep Inspiration")
        .expect("loaded Candlekeep Inspiration");
    let beacon = db
        .get_face_by_name("Beacon Bolt")
        .expect("loaded Beacon Bolt");
    let drake = db
        .get_face_by_name("Crackling Drake")
        .expect("loaded Crackling Drake");
    let huskburster = db
        .get_face_by_name("Huskburster Swarm")
        .expect("loaded Huskburster Swarm");
    let ral = db
        .get_face_by_name("Ral, Izzet Viceroy")
        .expect("loaded Ral, Izzet Viceroy");
    let curve = db
        .get_face_by_name("Serpentine Curve")
        .expect("loaded Serpentine Curve");
    for (face, name, oracle) in [
        (sailors, "Sailors' Bane", SAILORS),
        (candlekeep, "Candlekeep Inspiration", CANDLEKEEP),
        (beacon, "Beacon Bolt", BEACON),
        (drake, "Crackling Drake", DRAKE),
        (huskburster, "Huskburster Swarm", HUSKBURSTER),
        (ral, "Ral, Izzet Viceroy", RAL),
        (curve, "Serpentine Curve", CURVE),
    ] {
        assert_eq!(face.name, name);
        assert_eq!(face.oracle_text.as_deref(), Some(oracle));
    }

    // CR 108.3 + CR 109.2a + CR 109.5 + CR 400.1: the spell's "you"
    // selects ownership and the stated zones, independently of stored controller.
    let owned = FilterProp::Owned {
        controller: ControllerRef::You,
    };
    let typed =
        |kind, properties| TargetFilter::Typed(TypedFilter::new(kind).properties(properties));
    let zones = TargetFilter::Or {
        filters: vec![
            typed(
                TypeFilter::Card,
                vec![FilterProp::InZone { zone: Zone::Exile }],
            ),
            typed(
                TypeFilter::Card,
                vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                    owned.clone(),
                ],
            ),
        ],
    };
    let qa = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    typed(TypeFilter::Card, vec![owned.clone()]),
                    zones.clone(),
                    TargetFilter::Or {
                        filters: vec![
                            typed(TypeFilter::Instant, vec![]),
                            typed(TypeFilter::Sorcery, vec![]),
                            typed(TypeFilter::Card, vec![FilterProp::HasAdventure]),
                        ],
                    },
                ],
            },
        },
    };
    let qi = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Or {
                        filters: vec![
                            typed(TypeFilter::Instant, vec![owned.clone()]),
                            typed(TypeFilter::Sorcery, vec![owned.clone()]),
                        ],
                    },
                    zones.clone(),
                ],
            },
        },
    };
    let qc = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![typed(TypeFilter::Creature, vec![owned]), zones],
            },
        },
    };

    let sailors_static = sailors
        .static_abilities
        .first()
        .expect("Sailors cost static");
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(count),
        ..
    } = &sailors_static.mode
    else {
        panic!("Sailors dynamic cost slot");
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert_eq!(sailors_static.affected, Some(TargetFilter::SelfRef));
    let sailors_count = QuantityExpr::Ref { qty: count.clone() };
    assert_eq!(sailors_count, qa);

    let candlekeep_ability = candlekeep
        .abilities
        .first()
        .expect("Candlekeep spell ability");
    let Effect::GenericEffect {
        static_abilities,
        duration,
        ..
    } = candlekeep_ability.effect.as_ref()
    else {
        panic!("Candlekeep continuous effect slot");
    };
    // CR 611.2a + CR 613.4b: both base characteristics have the printed duration.
    assert_eq!(candlekeep_ability.duration, Some(Duration::UntilEndOfTurn));
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    let [candlekeep_static] = static_abilities.as_slice() else {
        panic!("one Candlekeep static");
    };
    assert_eq!(candlekeep_static.mode, StaticMode::Continuous);
    assert_eq!(
        candlekeep_static.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    assert!(!candlekeep_static.characteristic_defining);
    let [ContinuousModification::SetPowerDynamic {
        value: candlekeep_power,
    }, ContinuousModification::SetToughnessDynamic {
        value: candlekeep_toughness,
    }] = candlekeep_static.modifications.as_slice()
    else {
        panic!("exactly one distinct Candlekeep power and toughness slot");
    };
    assert_eq!(candlekeep_power, &qa);
    assert_eq!(candlekeep_toughness, &qa);

    let Effect::DealDamage {
        amount: beacon_count,
        ..
    } = beacon
        .abilities
        .first()
        .expect("Beacon ability")
        .effect
        .as_ref()
    else {
        panic!("Beacon damage amount slot");
    };
    assert_eq!(beacon_count, &qi);
    let drake_static = drake.static_abilities.first().expect("Drake power static");
    assert_eq!(drake_static.mode, StaticMode::Continuous);
    assert!(drake_static.characteristic_defining);
    assert_eq!(drake_static.affected, Some(TargetFilter::SelfRef));
    let [ContinuousModification::SetDynamicPower { value: drake_count }] =
        drake_static.modifications.as_slice()
    else {
        panic!("exactly one Drake dynamic power slot");
    };
    assert_eq!(drake_count, &qi);
    let huskburster_static = huskburster
        .static_abilities
        .first()
        .expect("Huskburster cost static");
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(count),
        ..
    } = &huskburster_static.mode
    else {
        panic!("Huskburster dynamic cost slot");
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert_eq!(huskburster_static.affected, Some(TargetFilter::SelfRef));
    let huskburster_count = QuantityExpr::Ref { qty: count.clone() };
    assert_eq!(huskburster_count, qc);
    let ral_ability = ral.abilities.get(1).expect("Ral minus-three ability");
    assert_eq!(ral_ability.cost, Some(AbilityCost::Loyalty { amount: -3 }));
    let Effect::DealDamage {
        amount: ral_count, ..
    } = ral_ability.effect.as_ref()
    else {
        panic!("Ral minus-three damage amount slot");
    };
    assert_eq!(ral_count, &qi);
    let Effect::Token {
        enter_with_counters,
        ..
    } = curve
        .abilities
        .first()
        .expect("Curve token ability")
        .effect
        .as_ref()
    else {
        panic!("Curve token counter slot");
    };
    let [(CounterType::Plus1Plus1, curve_count)] = enter_with_counters.as_slice() else {
        panic!("one Curve +1/+1 counter quantity");
    };
    assert_eq!(
        curve_count,
        &QuantityExpr::Offset {
            inner: Box::new(qi),
            offset: 1
        }
    );

    let slots = [
        ("Sailors cost", &sailors_count, 3, 0),
        ("Candlekeep power", candlekeep_power, 3, 0),
        ("Candlekeep toughness", candlekeep_toughness, 3, 0),
        ("Beacon damage", beacon_count, 2, 0),
        ("Drake power", drake_count, 2, 0),
        ("Huskburster cost", &huskburster_count, 2, 0),
        ("Ral damage", ral_count, 2, 0),
        ("Curve counters", curve_count, 3, 1),
    ];
    assert_eq!(slots.len(), 8);
    for populated in [true, false] {
        let mut scenario = GameScenario::new();
        let source = scenario
            .add_artifact_from_oracle(P0, "Loaded count source", "")
            .id();
        let mut owned_ids = Vec::new();
        let mut adventure_ids = Vec::new();
        if populated {
            let overlap = scenario
                .add_spell_to_exile(P0, "Loaded Instant Adventure", true)
                .id();
            let sorcery = scenario
                .add_spell_to_graveyard(P0, "Loaded Sorcery member", false)
                .id();
            let adventure = scenario
                .add_creature_to_graveyard(P0, "Loaded Adventure creature", 1, 1)
                .id();
            let ordinary = scenario
                .add_creature_to_graveyard(P0, "Loaded ordinary creature", 1, 1)
                .id();
            owned_ids.extend([overlap, sorcery, adventure, ordinary]);
            adventure_ids.extend([overlap, adventure]);
        }
        let foreign_ids = [
            scenario
                .add_spell_to_exile(P1, "Loaded foreign exile Instant", true)
                .id(),
            scenario
                .add_spell_to_graveyard(P1, "Loaded opponent Sorcery", false)
                .id(),
            scenario
                .add_creature_to_graveyard(P1, "Loaded opponent Creature", 1, 1)
                .id(),
        ];
        scenario.add_spell_to_hand(P0, "Loaded hand Instant", true);
        scenario.add_spell_to_library_top(P0, "Loaded library Instant", true);
        let mut runner = scenario.build();
        // CR 109.4: off-zone controller storage is not ownership authority.
        for id in owned_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
        }
        for id in foreign_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P0;
        }
        // CR 715.2a + CR 715.2c: real alternative characteristics qualify the
        // Adventure creature; the Instant plus Adventure remains one card.
        for id in adventure_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
                name: "Loaded Adventure alternative".into(),
                layout_kind: Some(LayoutKind::Adventure),
                ..Default::default()
            });
        }
        for (slot, quantity, positive, empty) in slots {
            assert_eq!(
                resolve_quantity(runner.state(), quantity, P0, source),
                if populated { positive } else { empty },
                "{slot}, populated={populated}"
            );
        }
    }
}
