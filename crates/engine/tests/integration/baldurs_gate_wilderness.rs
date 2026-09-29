//! Baldur's Gate Wilderness as the experimental dungeon pool.
//!
//! With `FormatConfig.allow_experimental_dungeons`, a normal venture offers
//! Baldur's Gate Wilderness alongside the AFR trio (CR 701.49a), and taking
//! the initiative (CR 726.2) offers it as an alternative to Undercity instead
//! of auto-entering the Undercity. With the flag off, both flows behave
//! exactly as before.
//!
//! The room tests below then walk every one of the Wilderness's 19 rooms and
//! resolve its trigger through the production pipeline, asserting the real
//! game outcome — not just the built ability's shape.

use std::collections::BTreeSet;

use engine::game::dungeon::{dungeon_sentinel_id, DungeonId, DungeonProgress};
use engine::game::effects::venture;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::player::PlayerId;

fn experimental_config() -> FormatConfig {
    let mut config = FormatConfig::standard();
    config.allow_experimental_dungeons = true;
    config
}

fn experimental_scenario() -> GameScenario {
    GameScenario::new_with_format(experimental_config(), 2, 42)
}

/// A synthetic driver ability for invoking the venture pipeline directly.
/// `venture::resolve` only reads `controller` (and `source_id` for the
/// resolution event), so the effect body is never consulted.
fn venture_ability(controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Unimplemented {
            name: "test venture driver".to_string(),
            description: None,
        },
        vec![],
        dungeon_sentinel_id(controller),
        controller,
    )
}

fn resolve_venture(runner: &mut GameRunner, player: PlayerId) {
    let ability = venture_ability(player);
    let mut events: Vec<GameEvent> = Vec::new();
    venture::resolve(runner.state_mut(), &ability, &mut events).expect("venture resolves");
}

fn resolve_take_initiative(runner: &mut GameRunner, player: PlayerId) {
    let ability = venture_ability(player);
    let mut events: Vec<GameEvent> = Vec::new();
    venture::resolve_take_initiative(runner.state_mut(), &ability, &mut events)
        .expect("take initiative resolves");
}

fn current_dungeon(runner: &GameRunner, player: PlayerId) -> Option<DungeonId> {
    runner
        .state()
        .dungeon_progress
        .get(&player)
        .and_then(|progress| progress.current_dungeon)
}

/// Position P0's venture marker at `room` of the Wilderness, as if reached
/// through normal play. The following venture advances out of it.
fn position_marker(runner: &mut GameRunner, room: u8) {
    runner.state_mut().dungeon_progress.insert(
        P0,
        DungeonProgress {
            current_dungeon: Some(DungeonId::BaldursGateWilderness),
            current_room: room,
            completed: BTreeSet::new(),
        },
    );
}

fn choose_dungeon_option_ids(runner: &GameRunner) -> Vec<DungeonId> {
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseDungeon { options, .. } => {
            options.into_iter().map(|option| option.dungeon).collect()
        }
        other => panic!("expected a ChooseDungeon prompt, got {other:?}"),
    }
}

// ─── Choice-level behavior ───────────────────────────────────────────────

#[test]
fn experimental_venture_offers_the_wilderness_alongside_the_afr_trio() {
    let scenario = experimental_scenario();
    let mut runner = scenario.build();

    resolve_venture(&mut runner, P0);

    assert_eq!(
        choose_dungeon_option_ids(&runner),
        vec![
            DungeonId::LostMineOfPhandelver,
            DungeonId::DungeonOfTheMadMage,
            DungeonId::TombOfAnnihilation,
            DungeonId::BaldursGateWilderness,
        ]
    );

    runner
        .act(GameAction::ChooseDungeon {
            dungeon: DungeonId::BaldursGateWilderness,
        })
        .expect("choose the Wilderness");
    assert_eq!(
        current_dungeon(&runner, P0),
        Some(DungeonId::BaldursGateWilderness)
    );
    assert!(
        !runner.state().stack.is_empty(),
        "entering the Wilderness must queue the Crash Landing trigger"
    );
}

#[test]
fn standard_venture_omits_the_wilderness() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    resolve_venture(&mut runner, P0);

    let options = choose_dungeon_option_ids(&runner);
    assert_eq!(options.len(), 3);
    assert!(!options.contains(&DungeonId::BaldursGateWilderness));
    assert!(!options.contains(&DungeonId::Undercity));
}

// ─── Venture topology ───────────────────────────────────────────────────

fn choose_room_option_ids(runner: &GameRunner) -> Vec<u8> {
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseDungeonRoom { options, .. } => {
            options.into_iter().map(|option| option.index).collect()
        }
        other => panic!("expected a ChooseDungeonRoom prompt, got {other:?}"),
    }
}

fn current_room(runner: &GameRunner) -> Option<u8> {
    runner
        .state()
        .dungeon_progress
        .get(&P0)
        .map(|progress| progress.current_room)
}

/// CR 309.5a: Crash Landing has three printed doors — Goblin Camp, Emerald
/// Grove, AND Auntie's Teahouse. Steer down the rightmost one and confirm the
/// single-exit room beyond auto-advances with no further prompt.
#[test]
fn wilderness_crash_landing_branches_three_ways() {
    use engine::types::card_type::{CoreType, Supertype};

    let mut scenario = experimental_scenario();
    let plains = scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Card C");
    scenario.add_card_to_library_top(P0, "Card B");
    scenario.add_card_to_library_top(P0, "Card A");
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&plains).unwrap();
    object.card_types.core_types.push(CoreType::Land);
    object.card_types.supertypes.push(Supertype::Basic);
    object.base_card_types = object.card_types.clone();

    enter_room(&mut runner, 0);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::SearchChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("take the found land");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    resolve_venture(&mut runner, P0);
    assert_eq!(
        choose_room_option_ids(&runner),
        vec![1, 2, 3],
        "Crash Landing offers all three printed exits"
    );
    runner
        .act(GameAction::ChooseDungeonRoom { room_index: 3 })
        .expect("take the Auntie's Teahouse door");
    assert_eq!(current_room(&runner), Some(3));
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ScryChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("scry everything to the top");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    // Auntie's Teahouse has one printed exit (Mountain Pass): no prompt.
    resolve_venture(&mut runner, P0);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseDungeonRoom { .. }
        ),
        "single-exit rooms auto-advance, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(current_room(&runner), Some(5));
}

/// CR 309.5a: Goblin Camp's only printed arrow goes to Defiled Temple — no
/// branch prompt, no Auntie's Teahouse detour.
#[test]
fn wilderness_goblin_camp_has_a_single_exit() {
    let scenario = experimental_scenario();
    let mut runner = scenario.build();
    position_marker(&mut runner, 1);

    resolve_venture(&mut runner, P0);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseDungeonRoom { .. }
        ),
        "Goblin Camp must not branch, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(current_room(&runner), Some(4));
}

/// CR 701.49c: the bottom row holds THREE terminal rooms. Venturing onward
/// from any of them completes the Wilderness and offers a new dungeon.
#[test]
fn wilderness_bottom_row_rooms_each_complete_the_dungeon() {
    for bottommost in [16, 17, 18] {
        let scenario = experimental_scenario();
        let mut runner = scenario.build();
        position_marker(&mut runner, bottommost);

        resolve_venture(&mut runner, P0);

        let progress = runner.state().dungeon_progress.get(&P0).cloned();
        assert_eq!(
            progress.as_ref().and_then(|p| p.current_dungeon),
            None,
            "room {bottommost} completes the dungeon: no active dungeon remains"
        );
        assert!(
            progress
                .map(|p| p.completed.contains(&DungeonId::BaldursGateWilderness))
                .unwrap_or(false),
            "room {bottommost} records the Wilderness as completed"
        );
        // Fresh offer after completion: the engine re-offers the whole pool
        // (see `venture_at_bottommost_completes_and_offers_new_dungeon`,
        // which re-offers all 3 after completing Lost Mine). Experimental
        // pool = AFR trio + Wilderness.
        let options = choose_dungeon_option_ids(&runner);
        assert_eq!(
            options,
            vec![
                DungeonId::LostMineOfPhandelver,
                DungeonId::DungeonOfTheMadMage,
                DungeonId::TombOfAnnihilation,
                DungeonId::BaldursGateWilderness,
            ],
            "room {bottommost} completion offers the full pool, got {options:?}"
        );
    }
}

#[test]
fn experimental_initiative_offers_wilderness_or_undercity() {
    let scenario = experimental_scenario();
    let mut runner = scenario.build();

    resolve_take_initiative(&mut runner, P0);

    assert_eq!(runner.state().initiative, Some(P0));
    assert_eq!(
        choose_dungeon_option_ids(&runner),
        vec![DungeonId::Undercity, DungeonId::BaldursGateWilderness]
    );

    runner
        .act(GameAction::ChooseDungeon {
            dungeon: DungeonId::BaldursGateWilderness,
        })
        .expect("choose the Wilderness");
    assert_eq!(
        current_dungeon(&runner, P0),
        Some(DungeonId::BaldursGateWilderness)
    );
}

#[test]
fn standard_initiative_still_auto_enters_undercity() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    resolve_take_initiative(&mut runner, P0);

    assert_eq!(runner.state().initiative, Some(P0));
    assert_eq!(current_dungeon(&runner, P0), Some(DungeonId::Undercity));
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ChooseDungeon { .. }),
        "without the flag, initiative must not prompt: {:?}",
        runner.state().waiting_for
    );
}

// ─── Room-by-room resolution ─────────────────────────────────────────────

/// P0's parent room for each Wilderness room, chosen to exercise both the
/// branch prompt and the auto-advance path across the suite. Every entry is a
/// printed arrow (see the `next_rooms` table); the independent edge test in
/// `dungeon.rs` pins the full graph separately, so this map cannot silently
/// agree with a wrong table.
fn parent_of(room: u8) -> u8 {
    match room {
        1..=3 => 0,
        4 => 2,
        5 => 3,
        6 => 4,
        7 | 8 => 5,
        9 => 6,
        10 => 8,
        11 => 9,
        12 | 13 => 10,
        14 => 11,
        15 => 13,
        16 => 14,
        17 | 18 => 15,
        _ => panic!("no parent for room {room}"),
    }
}

/// Venture P0 into `room` of the Wilderness through the production pipeline:
/// position at its parent (or hold no dungeon for room 0), venture, and
/// steer through any branch prompt. Leaves the room trigger on the stack.
fn enter_room(runner: &mut GameRunner, room: u8) {
    if room == 0 {
        resolve_venture(runner, P0);
        assert!(
            choose_dungeon_option_ids(runner).contains(&DungeonId::BaldursGateWilderness),
            "room 0 test requires the experimental pool"
        );
        runner
            .act(GameAction::ChooseDungeon {
                dungeon: DungeonId::BaldursGateWilderness,
            })
            .expect("choose the Wilderness");
    } else {
        position_marker(runner, parent_of(room));
        resolve_venture(runner, P0);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseDungeonRoom { .. }
        ) {
            runner
                .act(GameAction::ChooseDungeonRoom { room_index: room })
                .expect("steer into the room");
        }
    }
    let current = runner
        .state()
        .dungeon_progress
        .get(&P0)
        .map(|progress| progress.current_room);
    assert_eq!(current, Some(room), "must land in room {room}");
    assert!(
        !runner.state().stack.is_empty(),
        "entering room {room} must queue its trigger"
    );
}

/// Pass priority until the stack is empty. Panics on any prompt — rooms
/// with choices drive their own loops. Note the prompt check comes first:
/// some continuations (e.g. a search choice) pause with an empty stack.
fn drain(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    panic!("stack did not empty");
}

fn p0_tokens(runner: &GameRunner) -> Vec<engine::types::identifiers::ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|object| object.is_token && object.controller == P0)
        })
        .copied()
        .collect()
}

#[test]
fn room_01_goblin_camp_creates_a_treasure() {
    let mut runner = experimental_scenario().build();
    enter_room(&mut runner, 1);
    drain(&mut runner);

    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 1);
    let token = &runner.state().objects[&tokens[0]];
    assert!(token.card_types.subtypes.iter().any(|s| s == "Treasure"));
}

#[test]
fn room_02_emerald_grove_creates_a_2_2_white_knight() {
    let mut runner = experimental_scenario().build();
    enter_room(&mut runner, 2);
    drain(&mut runner);

    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 1);
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!(token.power, Some(2));
    assert_eq!(token.toughness, Some(2));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Knight"));
    assert!(token.color.contains(&engine::types::mana::ManaColor::White));
}

#[test]
fn room_06_ebonlake_grotto_creates_two_faerie_dragons() {
    let mut runner = experimental_scenario().build();
    enter_room(&mut runner, 6);
    drain(&mut runner);

    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 2);
    for id in tokens {
        let token = &runner.state().objects[&id];
        assert_eq!(token.power, Some(1));
        assert_eq!(token.toughness, Some(1));
        assert!(token.card_types.subtypes.iter().any(|s| s == "Faerie"));
        assert!(token.card_types.subtypes.iter().any(|s| s == "Dragon"));
        assert!(token.color.contains(&engine::types::mana::ManaColor::Blue));
        assert!(token
            .keywords
            .contains(&engine::types::keywords::Keyword::Flying));
    }
}

#[test]
fn room_09_last_light_inn_draws_two_cards() {
    let mut scenario = experimental_scenario();
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Forest");
    scenario.add_card_to_library_top(P0, "Mountain");
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();
    enter_room(&mut runner, 9);
    drain(&mut runner);

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 2,
        "Last Light Inn draws two cards"
    );
}

#[test]
fn room_12_gauntlet_of_shar_drains_each_opponent_for_5() {
    let mut runner = experimental_scenario().build();
    let p0_life = runner.state().players[0].life;
    let p1_life = runner.state().players[1].life;
    enter_room(&mut runner, 12);
    drain(&mut runner);

    assert_eq!(runner.state().players[0].life, p0_life);
    assert_eq!(runner.state().players[1].life, p1_life - 5);
}

#[test]
fn room_15_undercity_ruins_creates_three_skeletons() {
    let mut runner = experimental_scenario().build();
    enter_room(&mut runner, 15);
    drain(&mut runner);

    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 3);
    for id in tokens {
        let token = &runner.state().objects[&id];
        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(1));
        assert!(token.card_types.subtypes.iter().any(|s| s == "Skeleton"));
        assert!(token.color.contains(&engine::types::mana::ManaColor::Black));
        assert!(token
            .keywords
            .contains(&engine::types::keywords::Keyword::Menace));
    }
}

#[test]
fn room_10_reithwin_tollhouse_creates_2d4_treasures() {
    let mut runner = experimental_scenario().build();
    enter_room(&mut runner, 10);
    drain(&mut runner);

    let treasures = p0_tokens(&runner)
        .into_iter()
        .filter(|id| {
            runner.state().objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|s| s == "Treasure")
        })
        .count();
    assert!(
        (2..=8).contains(&treasures),
        "2d4 must yield 2-8 Treasures, got {treasures}"
    );
}

#[test]
fn room_16_steel_watch_foundry_emblem_pumps_your_team() {
    let mut scenario = experimental_scenario();
    let bear = scenario.add_creature(P0, "Emblem Bear", 2, 2).id();
    let mut runner = scenario.build();
    enter_room(&mut runner, 16);
    drain(&mut runner);

    let object = &runner.state().objects[&bear];
    assert_eq!(object.power, Some(4));
    assert_eq!(object.toughness, Some(4));
    assert!(object
        .keywords
        .contains(&engine::types::keywords::Keyword::Trample));
}

#[test]
fn room_18_temple_of_bhaal_shrinks_only_opponents_creatures() {
    let mut scenario = experimental_scenario();
    let foe = scenario.add_creature(P1, "Doomed Giant", 6, 6).id();
    let friend = scenario.add_creature(P0, "Safe Bear", 2, 2).id();
    let mut runner = scenario.build();
    enter_room(&mut runner, 18);
    drain(&mut runner);

    let foe_object = &runner.state().objects[&foe];
    assert_eq!(foe_object.power, Some(1));
    assert_eq!(foe_object.toughness, Some(1));
    let friend_object = &runner.state().objects[&friend];
    assert_eq!(friend_object.power, Some(2));
    assert_eq!(friend_object.toughness, Some(2));
}

#[test]
fn room_17_ansurs_sanctum_draws_four_and_drains_their_mana_value() {
    use engine::types::mana::ManaCost;

    let mut scenario = experimental_scenario();
    // Top four cost 1+2+3+4: the drain must be exactly 10. (Added top-down:
    // each call pushes onto library[0], so add the bottom card first.)
    for (index, cost) in [5u32, 4, 3, 2, 1].into_iter().enumerate() {
        scenario
            .add_spell_to_library_top(P0, &format!("Ansur Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(cost));
    }
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();
    let p0_life = runner.state().players[0].life;
    let p1_life = runner.state().players[1].life;

    enter_room(&mut runner, 17);
    drain(&mut runner);

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 4,
        "Ansur's Sanctum puts the four revealed cards into hand"
    );
    assert_eq!(
        runner.state().players[1].life,
        p1_life - 10,
        "each opponent loses life equal to the revealed total mana value (1+2+3+4)"
    );
    assert_eq!(
        runner.state().players[0].life,
        p0_life,
        "the controller loses nothing"
    );
}

#[test]
fn room_00_crash_landing_tutors_a_basic_land_to_hand() {
    use engine::types::card_type::{CoreType, Supertype};

    let mut scenario = experimental_scenario();
    let plains = scenario.add_card_to_library_top(P0, "Plains");
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&plains).unwrap();
    object.card_types.core_types.push(CoreType::Land);
    object.card_types.supertypes.push(Supertype::Basic);
    object.base_card_types = object.card_types.clone();
    let hand_before = runner.state().players[0].hand.len();

    enter_room(&mut runner, 0);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards, vec![plains], "only the basic land is offered");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![plains],
                    })
                    .expect("take the Plains");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "Crash Landing puts the found basic land into hand"
    );
    assert!(
        runner.state().players[0].hand.contains(&plains),
        "the Plains itself must be in hand"
    );
}

#[test]
fn room_03_aunties_teahouse_scries_3() {
    let mut scenario = experimental_scenario();
    let bottom_filler = scenario.add_card_to_library_top(P0, "Bottom Filler");
    let card_c = scenario.add_card_to_library_top(P0, "Card C");
    let card_b = scenario.add_card_to_library_top(P0, "Card B");
    let card_a = scenario.add_card_to_library_top(P0, "Card A");
    let mut runner = scenario.build();

    enter_room(&mut runner, 3);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ScryChoice { cards, .. } => {
                assert_eq!(cards, vec![card_a, card_b, card_c], "scry 3 looks at three");
                // Keep only C on top; A and B go to the bottom.
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![card_c],
                    })
                    .expect("scry C to top");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    let library = &runner.state().players[0].library;
    assert_eq!(library[0], card_c, "C stays on top");
    assert_eq!(library[1], bottom_filler);
    assert_eq!(library[2], card_a, "A goes to the bottom first");
    assert_eq!(library[3], card_b, "B goes to the bottom second");
}

#[test]
fn room_04_defiled_temple_sacrifices_to_draw() {
    let mut scenario = experimental_scenario();
    let fodder = scenario.add_creature(P0, "Sac Fodder", 1, 1).id();
    scenario.add_card_to_library_top(P0, "Draw Me");
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    enter_room(&mut runner, 4);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the sacrifice");
            }
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrifice the fodder");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects[&fodder].zone,
        engine::types::zones::Zone::Graveyard,
        "the fodder is sacrificed"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "sacrificing draws a card"
    );
}

#[test]
fn room_04_defiled_temple_decline_draws_nothing() {
    let mut scenario = experimental_scenario();
    let fodder = scenario.add_creature(P0, "Sac Fodder", 1, 1).id();
    scenario.add_card_to_library_top(P0, "Draw Me");
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    enter_room(&mut runner, 4);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the sacrifice");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects[&fodder].zone,
        engine::types::zones::Zone::Battlefield,
        "declining sacrifices nothing"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "declining draws nothing"
    );
}

#[test]
fn room_05_mountain_pass_puts_a_land_onto_the_battlefield() {
    let mut scenario = experimental_scenario();
    let land = scenario.add_land_to_hand(P0, "Hand Land").id();
    let mut runner = scenario.build();

    enter_room(&mut runner, 5);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the land drop");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects[&land].zone,
        engine::types::zones::Zone::Battlefield,
        "the land enters the battlefield"
    );
}

#[test]
fn room_05_mountain_pass_decline_leaves_the_hand_alone() {
    let mut scenario = experimental_scenario();
    let land = scenario.add_land_to_hand(P0, "Hand Land").id();
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    enter_room(&mut runner, 5);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the land drop");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects[&land].zone,
        engine::types::zones::Zone::Hand,
        "the land stays in hand"
    );
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
}

#[test]
fn room_07_grymforge_goads_the_opponents_creature() {
    use engine::types::ability::TargetRef;

    let mut scenario = experimental_scenario();
    let bear = scenario.add_creature(P1, "Victim Bear", 2, 2).id();
    let mut runner = scenario.build();

    enter_room(&mut runner, 7);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bear)),
                    })
                    .expect("goad the bear");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert!(
        runner.state().objects[&bear].goaded_by.contains(&P0),
        "Grymforge goads the chosen creature"
    );
}

#[test]
fn room_07_grymforge_decline_goads_nothing() {
    let mut scenario = experimental_scenario();
    let bear = scenario.add_creature(P1, "Victim Bear", 2, 2).id();
    let mut runner = scenario.build();

    enter_room(&mut runner, 7);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget { target: None })
                    .expect("decline to goad");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert!(
        runner.state().objects[&bear].goaded_by.is_empty(),
        "declining goads nothing"
    );
}

#[test]
fn room_07_grymforge_goads_one_creature_per_opponent_at_three_players() {
    let mut config = FormatConfig::commander();
    config.allow_experimental_dungeons = true;
    let mut scenario = GameScenario::new_with_format(config, 3, 42);
    let p2 = PlayerId(2);
    let bear1 = scenario.add_creature(P1, "Victim Bear One", 2, 2).id();
    let bear2 = scenario.add_creature(p2, "Victim Bear Two", 2, 2).id();
    let mut runner = scenario.build();

    enter_room(&mut runner, 7);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                // Answer every slot with its first legal target: each
                // opponent, then one of their creatures.
                let pick = match &runner.state().waiting_for {
                    WaitingFor::TriggerTargetSelection { selection, .. } => {
                        selection.current_legal_targets.first().cloned()
                    }
                    _ => None,
                };
                runner
                    .act(GameAction::ChooseTarget { target: pick })
                    .expect("answer the slot");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert!(
        runner.state().objects[&bear1].goaded_by.contains(&P0),
        "first opponent's creature is goaded"
    );
    assert!(
        runner.state().objects[&bear2].goaded_by.contains(&P0),
        "second opponent's creature is goaded"
    );
}

#[test]
fn room_08_githyanki_creche_distributes_three_counters() {
    use engine::types::ability::TargetRef;
    use engine::types::counter::CounterType;

    let mut scenario = experimental_scenario();
    let bear1 = scenario.add_creature(P0, "Counter Bear One", 2, 2).id();
    let bear2 = scenario.add_creature(P0, "Counter Bear Two", 2, 2).id();
    let mut runner = scenario.build();
    let mut picked: Vec<TargetRef> = Vec::new();

    enter_room(&mut runner, 8);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                // Pick each bear once, then decline further targets.
                let next = [bear1, bear2]
                    .into_iter()
                    .map(TargetRef::Object)
                    .find(|candidate| !picked.contains(candidate));
                if let Some(target) = next.clone() {
                    picked.push(target);
                }
                runner
                    .act(GameAction::ChooseTarget { target: next })
                    .expect("answer the target slot");
            }
            WaitingFor::DistributeAmong { total, .. } => {
                assert_eq!(total, 3, "three counters to distribute");
                runner
                    .act(GameAction::DistributeAmong {
                        distribution: vec![
                            (TargetRef::Object(bear1), 2),
                            (TargetRef::Object(bear2), 1),
                        ],
                    })
                    .expect("split 2/1");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects[&bear1]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2),
        "first bear gets two counters"
    );
    assert_eq!(
        runner.state().objects[&bear2]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "second bear gets one counter"
    );
}

#[test]
fn room_11_moonrise_towers_discounts_only_instant_and_sorcery_spells() {
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;

    let mut scenario = experimental_scenario();
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario
        .add_spell_to_hand(P0, "Moonrise Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Moonrise Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let mut runner = scenario.build();

    enter_room(&mut runner, 11);
    drain(&mut runner);

    let room_grant = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|effect| effect.source_id == dungeon_sentinel_id(P0))
        .expect("Moonrise Towers installs a transient cost grant");
    assert_eq!(
        room_grant.source_name, "Baldur's Gate Wilderness",
        "the production room trigger captures its dungeon name at installation"
    );

    // The {2}{R} instant is reduced to {R}: exactly {R} casts it.
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        engine::types::identifiers::ObjectId(0),
        false,
        vec![],
    ));
    let outcome = runner.cast(bolt).resolve();
    assert_eq!(
        outcome.zone_of(bolt),
        engine::types::zones::Zone::Graveyard,
        "the discounted instant resolves"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the instant costs exactly the reduced {{R}}"
    );

    // The {2}{G} creature is NOT reduced: exact {2}{G} empties the pool.
    for unit in [ManaType::Colorless, ManaType::Colorless, ManaType::Green] {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            unit,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    let outcome = runner.cast(bear).resolve();
    assert_eq!(
        outcome.zone_of(bear),
        engine::types::zones::Zone::Battlefield,
        "the creature resolves"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the creature pays full price"
    );
}

#[test]
fn room_13_balthazars_lab_returns_two_creatures_from_graveyard() {
    use engine::types::ability::TargetRef;

    let mut scenario = experimental_scenario();
    let body1 = scenario
        .add_creature_to_graveyard(P0, "Grave Body One", 2, 2)
        .id();
    let body2 = scenario
        .add_creature_to_graveyard(P0, "Grave Body Two", 3, 3)
        .id();
    let mut runner = scenario.build();
    let mut picked: Vec<TargetRef> = Vec::new();

    enter_room(&mut runner, 13);
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let next = [body1, body2]
                    .into_iter()
                    .map(TargetRef::Object)
                    .find(|candidate| !picked.contains(candidate));
                if let Some(target) = next.clone() {
                    picked.push(target);
                }
                runner
                    .act(GameAction::ChooseTarget { target: next })
                    .expect("answer the target slot");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    for body in [body1, body2] {
        assert_eq!(
            runner.state().objects[&body].zone,
            engine::types::zones::Zone::Hand,
            "both creatures return to hand"
        );
        assert!(
            runner.state().players[0].hand.contains(&body),
            "hand holds the returned creature"
        );
    }
}

#[test]
fn room_14_circus_copies_your_commander_without_legendary() {
    use engine::types::ability::TargetRef;
    use engine::types::card_type::Supertype;
    use engine::types::keywords::Keyword;

    let mut scenario = experimental_scenario();
    let commander = scenario
        .add_creature(P0, "Circus Commander", 3, 3)
        .commander()
        .id();
    let _bystander = scenario.add_creature(P0, "Bystander Bear", 2, 2).id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&commander).unwrap();
    object.card_types.supertypes.push(Supertype::Legendary);
    object.base_card_types = object.card_types.clone();
    // Shroud rejects every targeting reading: a commander that cannot be
    // targeted at all is still a legal resolution-time choice (CR 115.10a).
    object.keywords.push(Keyword::Shroud);
    object.base_keywords.push(Keyword::Shroud);

    enter_room(&mut runner, 14);
    let mut saw_choice = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ChooseObjectsSelection { eligible, .. } => {
                // No TriggerTargetSelection ever appears (it would panic
                // below): the choice happens during resolution, untargeted.
                // Only the commander is eligible — the bystander is excluded
                // by the IsCommander narrowing, and shroud excludes nothing.
                assert_eq!(
                    eligible,
                    vec![TargetRef::Object(commander)],
                    "only your commander can be chosen"
                );
                saw_choice = true;
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(commander)],
                    })
                    .expect("choose the commander");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(saw_choice, "the room must ask which commander to copy");

    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 1, "one commander token is created");
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!(token.power, Some(3));
    assert_eq!(token.toughness, Some(3));
    assert!(
        !token.card_types.supertypes.contains(&Supertype::Legendary),
        "the copy is not legendary"
    );
    assert!(
        runner.state().objects[&commander]
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "the original stays legendary"
    );
}
