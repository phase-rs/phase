//! Coordinated card populations through the production casting pipeline.
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card::LayoutKind;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

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
