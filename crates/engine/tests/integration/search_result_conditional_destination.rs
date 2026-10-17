use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameRng, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Complete Oracle text from MTGJSON AtomicCards.json; live parsing avoids stale fixtures.
const NAZAHN: &str = "When Nazahn enters, search your library for an Equipment card and reveal it. If you reveal a card named Hammer of Nazahn this way, put it onto the battlefield. Otherwise, put that card into your hand. Then shuffle.\nWhenever an equipped creature you control attacks, you may tap target creature defending player controls.";
const PATHMAKER: &str = "Vigilance\nWhen this Vehicle enters, you may search your library for an artifact card and reveal it. Put it onto the battlefield if its mana value is 2 or less. Otherwise, put it into your hand. If you search your library this way, shuffle.\nCrew 2";
const FARMER: &str = "When this creature enters, search your library for a basic Plains card and reveal it. If an opponent controls more lands than you, put it onto the battlefield tapped. Otherwise, put it into your hand. Then shuffle.\nForetell {1}{W} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const RUIN: &str = "Devoid (This card has no color.)\nSearch your library for a basic land card and reveal it. You may put that card onto the battlefield tapped if you control a land named Wastes. Otherwise, put that card into your hand. Then shuffle.";
const CHEST: &str = "{4}, Sacrifice this artifact: Roll a d20.\n1 | Trapped! — You lose 3 life.\n2—9 | Create five Treasure tokens.\n10—19 | You gain 3 life and draw three cards.\n20 | Search your library for a card. If it's an artifact card, you may put it onto the battlefield. Otherwise, put that card into your hand. Then shuffle.";
const CHARM: &str = "Choose one —\n• Search your library for a creature or land card and reveal it. Put it onto the battlefield tapped if it's a land card. Otherwise, put it into your hand. Then shuffle.\n• Put a +1/+1 counter on target creature you control. It deals damage equal to its power to target creature you don't control.\n• Exile target artifact or enchantment.";

fn fund(scenario: &mut GameScenario, generic: u32, colors: &[ManaType]) {
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(ManaType::Colorless, generic as usize)
            .chain(colors.iter().copied())
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
}

// This advances priority only. It never supplies an answer to a choice.
fn pass_to_pause(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> Vec<GameEvent> {
    let mut last = Vec::new();
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            return last;
        }
        last = runner.act(GameAction::PassPriority).unwrap().events;
        events.extend(last.clone());
    }
    panic!("priority did not reach a choice or settle");
}

fn assert_complete(runner: &GameRunner, last_action_events: &[GameEvent]) {
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(state.pending_search_found_batch.is_none());
    assert!(state.active_library_searches.is_empty());
    assert!(state.active_search_decision_controls.is_empty());
    // CR 608.2c + CR 701.24a: the final response must reach the printed shuffle.
    assert!(
        last_action_events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id: P0,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the last action must drain through shuffle: {last_action_events:?}"
    );
}

fn assert_search_prompt(runner: &GameRunner, expected: &[ObjectId], reveal: bool) {
    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        reveal: actual_reveal,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a search choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(cards.as_slice(), expected);
    assert_eq!(*count, 1);
    assert_eq!(*actual_reveal, reveal);
}

fn assert_optional_prompt(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "expected P0 optional decision, got {:?}",
        runner.state().waiting_for
    );
}

fn assert_library_order(runner: &GameRunner, expected: &[ObjectId]) {
    assert_eq!(
        runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        expected
    );
}

fn assert_search_reveal_and_delivery(
    runner: &GameRunner,
    events: &[GameEvent],
    source: ObjectId,
    selected: Option<(ObjectId, Zone)>,
    cover: ObjectId,
    revealing: bool,
) {
    assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == source
    )), "search must actually resolve: {events:?}");
    let reveals: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect();
    // CR 701.20a + CR 701.23b: reveal only the found card, never the hidden cover.
    assert_eq!(
        reveals,
        if revealing {
            selected.map(|(id, _)| vec![vec![id]]).unwrap_or_default()
        } else {
            vec![]
        }
    );
    if let Some((id, destination)) = selected {
        assert_eq!(runner.state().objects[&id].zone, destination);
        let moved = events
            .iter()
            .position(|event| {
                matches!(event,
                    GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to, .. }
                        if *object_id == id && *to == destination
                )
            })
            .expect("selected card must move from the searched library");
        let shuffle = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id: P0,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
            })
            .expect("printed shuffle must follow delivery");
        assert!(moved < shuffle);
        if revealing {
            let revealed = events
                .iter()
                .position(|event| matches!(event, GameEvent::CardsRevealed { .. }))
                .unwrap();
            assert!(
                revealed < moved,
                "reveal must precede the first destination move"
            );
        }
        if destination == Zone::Hand {
            assert!(!events.iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == id
            )), "the hand result must never transit the battlefield");
        }
    } else {
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    from: Some(Zone::Library),
                    ..
                }
            )),
            "an empty result cannot move any library card"
        );
    }
}

fn assert_pending_result(
    runner: &GameRunner,
    events: &[GameEvent],
    source: ObjectId,
    candidate: ObjectId,
) {
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&candidate].zone, Zone::Library);
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
    assert!(!events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), .. } if *object_id == candidate
    )));
}

fn pathmaker_setup(
    candidate_mv: Option<u32>,
) -> (GameRunner, ObjectId, Option<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_to_hand_from_oracle(P0, "Guidelight Pathmaker", PATHMAKER)
        .with_subtypes(vec!["Vehicle"])
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
        })
        .id();
    fund(&mut scenario, 4, &[ManaType::White, ManaType::Blue]);
    let candidate = candidate_mv.map(|mv| {
        scenario
            .add_creature_to_hand(P0, "Artifact candidate", 1, 1)
            .as_artifact()
            .with_mana_cost(ManaCost::generic(mv))
            .id()
    });
    let cover = scenario
        .add_spell_to_library_top(P0, "Nonartifact cover", false)
        .as_creature()
        .id();
    let mut runner = scenario.build();
    if let Some(id) = candidate {
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(id, Zone::Library),
            &mut Vec::new()
        ));
    }
    assert_library_order(
        &runner,
        &candidate
            .map(|id| vec![cover, id])
            .unwrap_or_else(|| vec![cover]),
    );
    (runner, source, candidate, cover)
}

fn start_pathmaker(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    runner.cast(source).commit();
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "the real six-mana cost must be paid"
    );
    let mut events = Vec::new();
    let _ = pass_to_pause(runner, &mut events);
    assert_optional_prompt(runner);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    events
}

/// CR 608.2c + CR 608.2h + CR 202.3: decide the destination after choosing the artifact.
#[test]
fn guidelight_search_choice_precedes_selected_artifact_destination() {
    for (mv, destination) in [(2, Zone::Battlefield), (3, Zone::Hand)] {
        let (mut runner, source, candidate, cover) = pathmaker_setup(Some(mv));
        let candidate = candidate.unwrap();
        let mut events = start_pathmaker(&mut runner, source);
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap()
                .events,
        );
        assert_search_prompt(&runner, &[candidate], true);
        assert_pending_result(&runner, &events, source, candidate);
        let last = runner
            .act(GameAction::SelectCards {
                cards: vec![candidate],
            })
            .unwrap()
            .events;
        events.extend(last.clone());
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_search_reveal_and_delivery(
            &runner,
            &events,
            source,
            Some((candidate, destination)),
            cover,
            true,
        );
    }
}

/// CR 608.2c: declining the search does not select an object to move.
/// TR2: settlement only, without a conditional-shuffle correctness claim.
#[test]
fn guidelight_declined_search_settles_without_moving_an_object() {
    let (mut runner, source, candidate, _) = pathmaker_setup(Some(2));
    start_pathmaker(&mut runner, source);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&candidate.unwrap()].zone,
        Zone::Library
    );
}

/// CR 701.23b + CR 701.20a: a performed empty search cannot reveal its cover.
#[test]
fn guidelight_performed_empty_search_keeps_the_hidden_cover_private() {
    let (mut runner, source, _, cover) = pathmaker_setup(None);
    let mut events = start_pathmaker(&mut runner, source);
    let last = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap()
        .events;
    events.extend(last.clone());
    assert_complete(&runner, &last);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
}

fn farmer_setup(
    opponent_more: bool,
    candidate: bool,
) -> (GameRunner, ObjectId, Option<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, "Stoic Farmer", 3, 3, FARMER)
        .with_subtypes(vec!["Dwarf", "Peasant"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White],
        })
        .id();
    fund(&mut scenario, 3, &[ManaType::White]);
    if opponent_more {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let found = candidate.then(|| scenario.add_basic_land(P0, ManaColor::White));
    let cover = scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();
    for id in [cover].into_iter().chain(found) {
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(id, Zone::Library),
            &mut Vec::new()
        ));
    }
    assert_library_order(
        &runner,
        &found
            .map(|id| vec![cover, id])
            .unwrap_or_else(|| vec![cover]),
    );
    (runner, source, found, cover)
}

/// CR 608.2c + CR 608.2h: the board gate waits for the selected Plains.
#[test]
fn stoic_farmer_search_settles_before_its_land_count_gate() {
    for more in [false, true] {
        let (mut runner, source, found, cover) = farmer_setup(more, true);
        let found = found.unwrap();
        runner.cast(source).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_search_prompt(&runner, &[found], true);
        assert_pending_result(&runner, &events, source, found);
        assert!(!runner.state().objects[&source].tapped);
        let last = runner
            .act(GameAction::SelectCards { cards: vec![found] })
            .unwrap()
            .events;
        events.extend(last.clone());
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert!(!runner.state().objects[&source].tapped);
        assert_search_reveal_and_delivery(
            &runner,
            &events,
            source,
            Some((found, if more { Zone::Battlefield } else { Zone::Hand })),
            cover,
            true,
        );
        assert_eq!(runner.state().objects[&found].tapped, more);
    }
}

/// CR 701.23b + CR 609.3: both board-gate outcomes drain with no found object.
#[test]
fn stoic_farmer_no_result_keeps_source_and_hidden_cover_untouched() {
    for more in [false, true] {
        let (mut runner, source, _, cover) = farmer_setup(more, false);
        runner.cast(source).commit();
        let mut events = Vec::new();
        let last = pass_to_pause(&mut runner, &mut events);
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert!(!runner.state().objects[&source].tapped);
        assert!(runner.state().players[0].hand.is_empty());
        assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
    }
}

fn ruin_setup(wastes: bool, candidate: bool) -> (GameRunner, ObjectId, Option<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "Ruin in Their Wake", false, RUIN)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    fund(&mut scenario, 1, &[ManaType::Green]);
    if wastes {
        scenario.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.");
    }
    let found = candidate.then(|| scenario.add_basic_land(P0, ManaColor::Green));
    let cover = scenario.add_land_to_library_top(P0, "Nonbasic cover").id();
    let mut runner = scenario.build();
    if let Some(id) = found {
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(id, Zone::Library),
            &mut Vec::new()
        ));
    }
    assert_library_order(
        &runner,
        &found
            .map(|id| vec![cover, id])
            .unwrap_or_else(|| vec![cover]),
    );
    (runner, source, found, cover)
}

/// CR 608.2c + CR 608.2h: the Wastes gate and optional decision use the chosen land.
#[test]
fn ruin_search_result_follows_wastes_and_optional_put_decision() {
    for (wastes, accept) in [(false, false), (true, true), (true, false)] {
        let (mut runner, source, found, cover) = ruin_setup(wastes, true);
        let found = found.unwrap();
        runner.cast(source).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_search_prompt(&runner, &[found], true);
        assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
        let mut last = runner
            .act(GameAction::SelectCards { cards: vec![found] })
            .unwrap()
            .events;
        events.extend(last.clone());
        if wastes {
            assert_optional_prompt(&runner);
            last = runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events;
            events.extend(last.clone());
        }
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        let destination = if wastes && accept {
            Zone::Battlefield
        } else {
            Zone::Hand
        };
        assert_search_reveal_and_delivery(
            &runner,
            &events,
            source,
            Some((found, destination)),
            cover,
            true,
        );
        assert_eq!(runner.state().objects[&found].tapped, wastes && accept);
    }
}

/// CR 701.23b + CR 609.3: an empty result cannot offer an impossible optional put.
#[test]
fn ruin_no_result_does_not_reveal_cover_or_offer_impossible_put() {
    for wastes in [false, true] {
        let (mut runner, source, _, cover) = ruin_setup(wastes, false);
        runner.cast(source).commit();
        let mut events = Vec::new();
        let last = pass_to_pause(&mut runner, &mut events);
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert!(runner.state().players[0].hand.is_empty());
        assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
    }
}

/// CR 701.23b + CR 608.2c: legal empty selections must resume every destination topology.
#[test]
fn conditional_search_empty_pick_drains_without_borrowing_a_result() {
    for more in [false, true] {
        let (mut runner, source, found, cover) = farmer_setup(more, true);
        runner.cast(source).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_search_prompt(&runner, &[found.unwrap()], true);
        assert_pending_result(&runner, &events, source, found.unwrap());
        let last = runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap()
            .events;
        events.extend(last.clone());
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert!(!runner.state().objects[&source].tapped);
        assert_eq!(runner.state().objects[&found.unwrap()].zone, Zone::Library);
        assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
    }
    let (mut runner, source, found, cover) = pathmaker_setup(Some(2));
    let mut events = start_pathmaker(&mut runner, source);
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap()
            .events,
    );
    assert_search_prompt(&runner, &[found.unwrap()], true);
    assert_pending_result(&runner, &events, source, found.unwrap());
    let last = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .unwrap()
        .events;
    events.extend(last.clone());
    assert_complete(&runner, &last);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&found.unwrap()].zone, Zone::Library);
    assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
    for wastes in [false, true] {
        let (mut runner, source, found, cover) = ruin_setup(wastes, true);
        runner.cast(source).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_search_prompt(&runner, &[found.unwrap()], true);
        assert_eq!(runner.state().objects[&found.unwrap()].zone, Zone::Library);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
        let last = runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap()
            .events;
        events.extend(last.clone());
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&found.unwrap()].zone, Zone::Library);
        assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
    }
}

/// CR 608.2c + CR 701.23a: the found Equipment never denotes Nazahn itself.
/// TR1 named-reveal destination remains unsupported; this asserts only source binding.
#[test]
fn nazahn_search_result_never_rebinds_to_the_entering_source() {
    for name in ["Other Equipment", "Hammer of Nazahn"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand_from_oracle(P0, "Nazahn, Revered Bladesmith", 5, 4, NAZAHN)
            .as_legendary()
            .with_subtypes(vec!["Cat", "Artificer"])
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green, ManaCostShard::White],
            })
            .id();
        fund(&mut scenario, 4, &[ManaType::Green, ManaType::White]);
        let found = scenario
            .add_creature_to_hand(P0, name, 1, 1)
            .as_artifact()
            .with_subtypes(vec!["Equipment"])
            .id();
        let cover = scenario
            .add_spell_to_library_top(P0, "Non-Equipment cover", false)
            .id();
        let mut runner = scenario.build();
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(found, Zone::Library),
            &mut Vec::new()
        ));
        assert_library_order(&runner, &[cover, found]);
        runner.cast(source).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_search_prompt(&runner, &[found], true);
        assert_pending_result(&runner, &events, source, found);
        let last = runner
            .act(GameAction::SelectCards { cards: vec![found] })
            .unwrap()
            .events;
        events.extend(last.clone());
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert!(events.iter().any(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == source
        )));
        let reveals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(reveals, vec![vec![found]]);
        let moved = events.iter().position(|event| matches!(event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), .. } if *object_id == found
        )).expect("the selected result must reach a move, not stop at an unsupported clause");
        let revealed = events
            .iter()
            .position(|event| matches!(event, GameEvent::CardsRevealed { .. }))
            .unwrap();
        let shuffled = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id: P0,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
            })
            .unwrap();
        assert!(revealed < moved && moved < shuffled);
    }
}

fn chest_setup(
    candidate_artifact: Option<bool>,
) -> (GameRunner, ObjectId, Option<ObjectId>, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Treasure Chest", CHEST)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    fund(&mut scenario, 4, &[]);
    let found = candidate_artifact.map(|artifact| {
        let mut builder = scenario.add_creature_to_hand(P0, "Chest candidate", 1, 1);
        if artifact {
            builder.as_artifact();
        }
        builder.id()
    });
    let cover = candidate_artifact.map(|_| {
        scenario
            .add_spell_to_library_top(P0, "Unselected cover", false)
            .id()
    });
    let mut runner = scenario.build();
    if let Some(id) = found {
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(id, Zone::Library),
            &mut Vec::new()
        ));
        assert_library_order(&runner, &[cover.unwrap(), id]);
    } else {
        assert_library_order(&runner, &[]);
    }
    (runner, source, found, cover)
}

fn activate_chest_twenty(
    runner: &mut GameRunner,
    source: ObjectId,
) -> (Vec<GameEvent>, Vec<GameEvent>) {
    // CR 602.2: activation pays the printed mana and sacrifice costs through the public action.
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap()
        .events;
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    assert!(runner.state().players[0].mana_pool.is_empty());
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. } if *object_id == source
    )), "sacrifice must be an actual paid activation cost");
    runner.state_mut().rng_seed = 19;
    runner.state_mut().rng_word_pos = 0;
    runner.state_mut().rng = GameRng::seed_from_u64(19);
    let last = pass_to_pause(runner, &mut events);
    // CR 706.2: the emitted final result is the table-selection authority.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::DieRolled {
                sides: 20,
                result: Some(20),
                ..
            }
        )),
        "seed 19 must actually reach result 20: {events:?}"
    );
    (events, last)
}

/// CR 608.2c: result 20 checks the unrevealed selected artifact, not the sacrificed Chest.
#[test]
fn treasure_chest_unrevealed_artifact_can_enter_the_battlefield() {
    let (mut runner, source, found, cover) = chest_setup(Some(true));
    let found = found.unwrap();
    let cover = cover.unwrap();
    let (mut events, _) = activate_chest_twenty(&mut runner, source);
    assert_search_prompt(&runner, &[cover, found], false);
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
    events.extend(
        runner
            .act(GameAction::SelectCards { cards: vec![found] })
            .unwrap()
            .events,
    );
    assert_optional_prompt(&runner);
    let last = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap()
        .events;
    events.extend(last.clone());
    assert_complete(&runner, &last);
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_search_reveal_and_delivery(
        &runner,
        &events,
        source,
        Some((found, Zone::Battlefield)),
        cover,
        false,
    );
}

/// CR 608.2c: a declined artifact put or nonartifact result goes straight to hand.
#[test]
fn treasure_chest_hand_destination_never_visits_the_battlefield() {
    for artifact in [true, false] {
        let (mut runner, source, found, cover) = chest_setup(Some(artifact));
        let found = found.unwrap();
        let cover = cover.unwrap();
        let (mut events, _) = activate_chest_twenty(&mut runner, source);
        assert_search_prompt(&runner, &[cover, found], false);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        let mut last = runner
            .act(GameAction::SelectCards { cards: vec![found] })
            .unwrap()
            .events;
        events.extend(last.clone());
        if artifact {
            assert_optional_prompt(&runner);
            last = runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap()
                .events;
            events.extend(last.clone());
        }
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_search_reveal_and_delivery(
            &runner,
            &events,
            source,
            Some((found, Zone::Hand)),
            cover,
            false,
        );
    }
}

/// CR 701.23d + CR 609.3: an unrestricted search may find nothing in an empty library.
#[test]
fn treasure_chest_empty_library_does_not_return_the_sacrificed_source() {
    let (mut runner, source, _, _) = chest_setup(None);
    let (events, last) = activate_chest_twenty(&mut runner, source);
    assert_complete(&runner, &last);
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(runner.state().players[0].hand.is_empty());
    assert!(!events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == source
    )));
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == source
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
}

/// CR 701.23b + CR 608.2c: modal direct destinations drain synchronous and chosen empty results.
#[test]
fn archdruids_charm_empty_result_drains_its_direct_conditional_destination() {
    for candidate in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_spell_to_hand_from_oracle(P0, "Archdruid's Charm", true, CHARM)
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Green; 3],
            })
            .id();
        fund(&mut scenario, 0, &[ManaType::Green; 3]);
        let found = candidate.then(|| {
            scenario
                .add_land_to_library_top(P0, "Forest")
                .with_subtypes(vec!["Forest"])
                .id()
        });
        let cover = scenario
            .add_spell_to_library_top(P0, "Noncreature nonland cover", false)
            .id();
        let mut runner = scenario.build();
        assert_library_order(
            &runner,
            &found
                .map(|id| vec![cover, id])
                .unwrap_or_else(|| vec![cover]),
        );
        runner.cast(source).modes(&[0]).commit();
        let mut events = Vec::new();
        let resolving_action = pass_to_pause(&mut runner, &mut events);
        let last = if let Some(id) = found {
            assert_search_prompt(&runner, &[id], true);
            let last = runner
                .act(GameAction::SelectCards { cards: vec![] })
                .unwrap()
                .events;
            events.extend(last.clone());
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
            last
        } else {
            resolving_action
        };
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_search_reveal_and_delivery(&runner, &events, source, None, cover, true);
    }
}
