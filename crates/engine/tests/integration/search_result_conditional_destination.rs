use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, AggregateFunction, AttachSelection,
    CardTypeSetSource, CastManaObjectScope, CastManaSpentMetric, ChosenTargetSlots, Comparator,
    ContinuousModification, ControllerRef, CountScope, DoubleTarget, Duration, Effect, EffectKind,
    FilterProp, LetterQuery, NameStickerSet, ObjectScope, PlayerFilter, PlayerRelation,
    PlayerScope, PossessionAxis, QuantityExpr, QuantityRef, SearchSelectionConstraint,
    StaticDefinition, SubAbilityLink, TargetChoiceTiming, TargetFilter, TargetReadOrigin,
    TargetRef, TypeFilter, TypedFilter, ZoneRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, ManaTapState, PlayerActionKind};
use engine::types::game_state::{GameRng, StackEntryKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef, TrackedSetId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerCounterKind;
use engine::types::statics::{CostModifyMode, CostReductionReach, StaticMode};
use engine::types::stickers::{AppliedSticker, StickerLocator};
use engine::types::zones::Zone;

// Complete Oracle text from MTGJSON AtomicCards.json; live parsing avoids stale fixtures.
const NAZAHN: &str = "When Nazahn enters, search your library for an Equipment card and reveal it. If you reveal a card named Hammer of Nazahn this way, put it onto the battlefield. Otherwise, put that card into your hand. Then shuffle.\nWhenever an equipped creature you control attacks, you may tap target creature defending player controls.";
const SELFIE: &str = "Search your library for a basic land card and reveal it. If there's a tree in its art, put it onto the battlefield tapped. Otherwise, put it into your hand. Then shuffle.";
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

fn assert_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.pending_search_found_batch.is_none());
    assert!(state.active_library_searches.is_empty());
    assert!(state.active_search_decision_controls.is_empty());
}

fn assert_complete(runner: &GameRunner, last_action_events: &[GameEvent]) {
    assert_settled(runner);
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
    assert_settled(&runner);
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
/// TR1 keeps BOTH named destinations unsupported. The supported prefix reveals
/// the selected card in its library and the independent shuffle still settles.
#[test]
fn nazahn_search_result_never_rebinds_to_the_entering_source() {
    for (name, select) in [
        (Some("Other Equipment"), true),
        (Some("Hammer of Nazahn"), true),
        (Some("Other Equipment"), false),
        (Some("Hammer of Nazahn"), false),
        (None, false),
    ] {
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
        let found = name.map(|name| {
            scenario
                .add_creature_to_hand(P0, name, 1, 1)
                .as_artifact()
                .with_subtypes(vec!["Equipment"])
                .id()
        });
        let cover = scenario
            .add_spell_to_library_top(P0, "Non-Equipment cover", false)
            .id();
        let mut runner = scenario.build();
        if let Some(found) = found {
            assert!(!move_object_for_test(
                runner.state_mut(),
                ZoneMoveRequest::pregame(found, Zone::Library),
                &mut Vec::new()
            ));
        }
        assert_library_order(
            &runner,
            &std::iter::once(cover).chain(found).collect::<Vec<_>>(),
        );
        runner.cast(source).commit();
        // CR 601.2h: this is the actual six-mana cast, before ETB resolution.
        assert!(runner.state().players[0].mana_pool.is_empty());
        assert_eq!(runner.state().objects[&source].zone, Zone::Stack);
        let mut events = Vec::new();
        let resolving_action = pass_to_pause(&mut runner, &mut events);
        let last = if let Some(found) = found {
            assert_search_prompt(&runner, &[found], true);
            assert_pending_result(&runner, &events, source, found);
            assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
            // CR 701.23b: a quality search permits an explicit empty selection.
            let last = runner
                .act(GameAction::SelectCards {
                    cards: if select { vec![found] } else { vec![] },
                })
                .unwrap()
                .events;
            events.extend(last.clone());
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
            assert!(
                !events.iter().any(|event| matches!(event,
                    GameEvent::ZoneChanged { object_id, .. } if *object_id == found
                )),
                "a refused conditional must supply neither destination"
            );
            last
        } else {
            // SearchLibrary's synchronous no-quality-match branch supplies no prompt.
            resolving_action
        };
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert!(!events.iter().any(|event| matches!(event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), .. } if *object_id == source
        )));
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    from: Some(Zone::Library),
                    ..
                }
            )),
            "neither the selected result nor the hidden cover may move"
        );
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
        // CR 701.20a + CR 701.20b: revealing only the selected card leaves it
        // in its library; this is gap safety, not full named-predicate support.
        assert_eq!(
            reveals,
            if select {
                vec![vec![found.unwrap()]]
            } else {
                vec![]
            }
        );
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
        if select {
            let revealed = events
                .iter()
                .position(|event| matches!(event, GameEvent::CardsRevealed { .. }))
                .unwrap();
            assert!(revealed < shuffled);
        }
    }
}

/// CR 701.20b + CR 701.23b: the real search/reveal and legal empty choices
/// survive while TR4's art predicate and BOTH destinations remain unsupported.
#[test]
fn selfie_preservation_refused_destinations_keep_the_supported_search_and_shuffle() {
    for (candidate, select) in [(true, true), (true, false), (false, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_spell_to_hand_from_oracle(P0, "Selfie Preservation", false, SELFIE)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        fund(&mut scenario, 1, &[ManaType::Green]);
        let found = candidate.then(|| scenario.add_basic_land(P0, ManaColor::Green));
        let cover = scenario
            .add_spell_to_library_top(P0, "Nonland cover", false)
            .id();
        let mut runner = scenario.build();
        if let Some(found) = found {
            assert!(!move_object_for_test(
                runner.state_mut(),
                ZoneMoveRequest::pregame(found, Zone::Library),
                &mut Vec::new()
            ));
        }
        assert_library_order(
            &runner,
            &std::iter::once(cover).chain(found).collect::<Vec<_>>(),
        );
        runner.cast(source).commit();
        // CR 601.2h: the actual {1}{G} is paid before the search begins.
        assert!(runner.state().players[0].mana_pool.is_empty());
        assert_eq!(runner.state().objects[&source].zone, Zone::Stack);
        let mut events = Vec::new();
        let resolving_action = pass_to_pause(&mut runner, &mut events);
        let last = if let Some(found) = found {
            assert_search_prompt(&runner, &[found], true);
            assert_eq!(runner.state().objects[&source].zone, Zone::Stack);
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
            assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
            assert!(!events.iter().any(|event| matches!(
                event,
                GameEvent::CardsRevealed { .. }
                    | GameEvent::ZoneChanged {
                        from: Some(Zone::Library),
                        ..
                    }
            )));
            let last = runner
                .act(GameAction::SelectCards {
                    cards: if select { vec![found] } else { vec![] },
                })
                .unwrap()
                .events;
            events.extend(last.clone());
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
            assert!(!events.iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == found
            )));
            last
        } else {
            resolving_action
        };
        assert_complete(&runner, &last);
        // CR 608.2n: final sorcery completion still puts Selfie in its graveyard.
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert!(events.iter().any(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == source
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                from: Some(Zone::Library),
                ..
            }
        )));
        let reveals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reveals,
            if select {
                vec![vec![found.unwrap()]]
            } else {
                vec![]
            }
        );
        if select {
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
            assert!(revealed < shuffled);
        }
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

/// CR 608.2c + CR 609.3: the independent printed Draw survives either search
/// decision. The searched land belongs to P1; the drawn card belongs to P0.
#[test]
fn declined_search_preserves_cleansing_wildfires_independent_draw() {
    const ORACLE: &str = "Destroy target land. Its controller may search their library for a basic land card, put it onto the battlefield tapped, then shuffle.\nDraw a card.";
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Cleansing Wildfire", false, ORACLE)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            })
            .id();
        fund(&mut scenario, 1, &[ManaType::Red]);
        let victim = scenario
            .add_land_from_oracle(P1, "Opponent nonbasic", "")
            .id();
        let found = scenario.add_basic_land(P1, ManaColor::Green);
        let cover = scenario
            .add_spell_to_library_top(P1, "P1 hidden cover", false)
            .id();
        let drawn = scenario
            .add_spell_to_library_top(P0, "Known independent draw", false)
            .id();
        let mut runner = scenario.build();
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(found, Zone::Library),
            &mut Vec::new()
        ));
        assert_eq!(
            runner.state().players[1]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![cover, found]
        );
        runner.cast(spell).target_object(victim).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged {
            object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, ..
        } if *object_id == victim)));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P1, .. }
        ));
        let mut last = runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap()
            .events;
        events.extend(last.clone());
        if accept {
            let WaitingFor::SearchChoice {
                player,
                cards,
                reveal,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("opponent search must pause");
            };
            assert_eq!(*player, P1);
            assert_eq!(cards, &[found]);
            assert!(!reveal);
            last = runner
                .act(GameAction::SelectCards { cards: vec![found] })
                .unwrap()
                .events;
            events.extend(last.clone());
            assert_eq!(runner.state().objects[&found].zone, Zone::Battlefield);
            assert!(runner.state().objects[&found].tapped);
            let moved = events
                .iter()
                .position(|event| {
                    matches!(event, GameEvent::ZoneChanged {
                object_id, from: Some(Zone::Library), to: Zone::Battlefield, ..
            } if *object_id == found)
                })
                .expect("selected land enters directly");
            let shuffled = events
                .iter()
                .position(|event| {
                    matches!(
                        event,
                        GameEvent::PlayerPerformedAction {
                            player_id: P1,
                            action: PlayerActionKind::ShuffledLibrary,
                            ..
                        }
                    )
                })
                .expect("successful opponent search shuffles");
            let draw = events
                .iter()
                .position(|event| {
                    matches!(event, GameEvent::CardDrawn {
                player_id: P0, object_id, ..
            } if *object_id == drawn)
                })
                .expect("independent draw follows search");
            assert!(moved < shuffled && shuffled < draw);
        } else {
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        }
        assert_settled(&runner);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![drawn]
        );
        assert_eq!(last.iter().filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, object_id, .. } if *object_id == drawn)).count(), 1);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
    }
}

/// CR 602.2 + CR 608.2c + CR 701.23b: declining the opponent search must leave
/// the activator's separate search and its own selected/empty result intact.
#[test]
fn demolition_field_later_search_owns_its_result_after_opponent_declines() {
    const ORACLE: &str = "{T}: Add {C}.\n{2}, {T}, Sacrifice this land: Destroy target nonbasic land an opponent controls. That land's controller may search their library for a basic land card, put it onto the battlefield, then shuffle. You may search your library for a basic land card, put it onto the battlefield, then shuffle.";
    for find in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_land_from_oracle(P0, "Demolition Field", ORACLE)
            .id();
        let victim = scenario
            .add_land_from_oracle(P1, "Opponent nonbasic", "")
            .id();
        let unchosen_nonbasic = scenario
            .add_land_from_oracle(P1, "Unchosen opponent nonbasic", "")
            .id();
        let first = scenario.add_basic_land(P1, ManaColor::White);
        let found = scenario.add_basic_land(P0, ManaColor::Green);
        let cover = scenario
            .add_spell_to_library_top(P0, "P0 nonland cover", false)
            .id();
        fund(&mut scenario, 2, &[]);
        let mut runner = scenario.build();
        for id in [first, found] {
            assert!(!move_object_for_test(
                runner.state_mut(),
                ZoneMoveRequest::pregame(id, Zone::Library),
                &mut Vec::new()
            ));
        }
        assert_library_order(&runner, &[cover, found]);
        let mut events = runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 1,
            })
            .unwrap()
            .events;
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Demolition Field must offer its printed target choice");
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert_eq!(selection.current_slot, 0);
        let legal = &target_slots[selection.current_slot].legal_targets;
        assert_eq!(legal.len(), 2);
        assert!(legal.contains(&TargetRef::Object(victim)));
        assert!(legal.contains(&TargetRef::Object(unchosen_nonbasic)));
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(victim)),
                })
                .unwrap()
                .events,
        );
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().stack.len(), 1);
        assert!(runner.state().players[0].mana_pool.is_empty());
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged {
            object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, ..
        } if *object_id == source)));
        assert!(events.iter().any(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source)), "activation paid its printed tap cost");
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&unchosen_nonbasic].zone,
            Zone::Battlefield
        );
        assert_eq!(runner.state().objects[&unchosen_nonbasic].controller, P1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P1, .. }
        ));
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap()
                .events,
        );
        assert_optional_prompt(&runner);
        assert_eq!(runner.state().objects[&first].zone, Zone::Library);
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap()
                .events,
        );
        assert_search_prompt(&runner, &[found], false);
        assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
        let last = runner
            .act(GameAction::SelectCards {
                cards: if find { vec![found] } else { vec![] },
            })
            .unwrap()
            .events;
        events.extend(last.clone());
        assert_complete(&runner, &last);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&unchosen_nonbasic].zone,
            Zone::Battlefield
        );
        assert_eq!(runner.state().objects[&unchosen_nonbasic].controller, P1);
        assert_eq!(runner.state().objects[&first].zone, Zone::Library);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        if find {
            assert_eq!(runner.state().objects[&found].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&found].controller, P0);
            assert!(!runner.state().objects[&found].tapped);
            assert!(last
                .iter()
                .any(|event| matches!(event, GameEvent::ZoneChanged {
                object_id, from: Some(Zone::Library), to: Zone::Battlefield, ..
            } if *object_id == found)));
        } else {
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
            assert!(!last.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    from: Some(Zone::Library),
                    ..
                }
            )));
        }
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
    }
}

// Synthetic typed API contracts: no printed card is claimed to combine these
// instructions. The bare tracked-set movement demands real search-finalizer
// publication, rather than assuming that a membership property publishes a set.
fn synthetic_optional_search(mut sibling: AbilityDefinition) -> AbilityDefinition {
    sibling.sub_link = SubAbilityLink::SequentialSibling;
    let shuffle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
    )
    .sub_ability(sibling);
    let mut movement: Effect = serde_json::from_value(serde_json::json!({
        "type":"ChangeZone", "origin":"Library", "destination":"Battlefield",
        "target":{"type":"TrackedSet","id":0}
    }))
    .unwrap();
    let Effect::ChangeZone {
        enter_with_counters,
        ..
    } = &mut movement
    else {
        unreachable!()
    };
    *enter_with_counters = vec![
        (CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 2 }),
        (CounterType::Lore, QuantityExpr::Fixed { value: 3 }),
    ];
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: true,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
        },
    )
    .optional()
    .sub_ability(AbilityDefinition::new(AbilityKind::Spell, movement).sub_ability(shuffle))
}

#[test]
fn declined_search_damage_all_keeps_independent_player_audience() {
    let empty_objects =
        TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::InTrackedSet {
                id: TrackedSetId(0),
            }]),
        );
    let live_objects = TargetFilter::Typed(TypedFilter::creature());
    let missing_amount = QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetSize,
    };
    let missing_players = PlayerFilter::ControlsCount {
        relation: PlayerRelation::All,
        filter: live_objects.clone(),
        comparator: Comparator::GE,
        count: Box::new(missing_amount.clone()),
    };
    for (label, target, player_filter, amount, object_damage, player_damage) in [
        (
            "empty tracked membership",
            empty_objects.clone(),
            Some(PlayerFilter::All),
            QuantityExpr::Fixed { value: 1 },
            0,
            1,
        ),
        (
            "literal empty object audience",
            TargetFilter::None,
            Some(PlayerFilter::All),
            QuantityExpr::Fixed { value: 1 },
            0,
            1,
        ),
        (
            "live object and player audiences",
            live_objects.clone(),
            Some(PlayerFilter::All),
            QuantityExpr::Fixed { value: 1 },
            1,
            1,
        ),
        (
            "empty objects without players",
            empty_objects.clone(),
            None,
            QuantityExpr::Fixed { value: 1 },
            0,
            0,
        ),
        (
            "missing required amount",
            live_objects.clone(),
            Some(PlayerFilter::All),
            missing_amount.clone(),
            0,
            0,
        ),
        (
            "missing required player threshold",
            live_objects.clone(),
            Some(missing_players),
            QuantityExpr::Fixed { value: 1 },
            0,
            0,
        ),
        (
            "missing required object threshold",
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: missing_amount,
            }])),
            Some(PlayerFilter::All),
            QuantityExpr::Fixed { value: 1 },
            0,
            0,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P0, "First live creature", 3, 3).id();
        let second = scenario.add_creature(P1, "Second live creature", 3, 3).id();
        let found = scenario
            .add_spell_to_library_top(P0, "Unchosen creature", false)
            .as_creature()
            .id();
        let cover = scenario
            .add_spell_to_library_top(P0, "Hidden cover", false)
            .id();
        let mut tail = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        let damage = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount,
                target,
                player_filter,
                damage_source: None,
            },
        )
        .sub_ability(tail);
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic search and simultaneous damage", false)
            .with_ability_definition(synthetic_optional_search(damage))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        assert_library_order(&runner, &[cover, found]);
        assert!(runner.state().last_revealed_ids.is_empty());
        runner.cast(spell).commit();
        assert!(runner.state().players[0].mana_pool.is_empty());
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(runner.state().players[1].life, 20);
        let last = runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap()
            .events;
        // CR 120.3a + CR 608.2c: apply each player's damage, then the independent +3.
        assert_eq!(
            runner.state().players[0].life,
            23 - player_damage,
            "{label}"
        );
        assert_eq!(
            runner.state().players[1].life,
            20 - player_damage,
            "{label}"
        );
        for id in [first, second] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().objects[&id].damage_marked,
                object_damage,
                "{label}"
            );
        }
        let damage: Vec<_> = last
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    target,
                    amount,
                    is_combat,
                    ..
                } => {
                    assert_eq!(*source_id, spell);
                    assert_eq!(*amount, 1);
                    assert!(!is_combat);
                    Some(target.clone())
                }
                _ => None,
            })
            .collect();
        for id in [first, second] {
            assert_eq!(
                damage.contains(&TargetRef::Object(id)),
                object_damage == 1,
                "{label}"
            );
        }
        for player in [P0, P1] {
            assert_eq!(
                damage.contains(&TargetRef::Player(player)),
                player_damage == 1,
                "{label}"
            );
        }
        assert_eq!(
            damage.len(),
            (2 * object_damage + 2 * player_damage as u32) as usize,
            "{label}"
        );
        if player_damage == 1 {
            // CR 608.2f: both audiences belong to one simultaneous instruction.
            assert_eq!(
                last.iter()
                    .filter(|event| matches!(event,
                        GameEvent::EffectResolved { kind: EffectKind::DamageAll, source_id, .. }
                            if *source_id == spell
                    ))
                    .count(),
                1,
                "{label}"
            );
        }
        assert_eq!(
            last.iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::LifeChanged {
                        player_id: P0,
                        amount: 3,
                        ..
                    }
                ))
                .count(),
            1,
            "{label}"
        );
        assert!(!last.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { .. }
                | GameEvent::ZoneChanged {
                    from: Some(Zone::Library),
                    ..
                }
        )));
        assert_library_order(&runner, &[cover, found]);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_keeps_described_counter_survivors() {
    for typed in [false, true] {
        for accept in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let survivor = scenario
                .add_artifact_from_oracle(P0, "Independent artifact", "")
                .id();
            let other = scenario.add_creature(P0, "Nonartifact creature", 3, 3).id();
            let another = scenario
                .add_creature(P0, "Second nonartifact creature", 3, 3)
                .id();
            let opponent = scenario
                .add_artifact_from_oracle(P1, "Opponent artifact", "")
                .id();
            for id in [survivor, other, another, opponent] {
                scenario
                    .with_counter(id, CounterType::Plus1Plus1, 2)
                    .with_counter(id, CounterType::Lore, 3);
            }
            let found = scenario
                .add_spell_to_library_top(P0, "Selected creature", false)
                .as_creature()
                .id();
            let cover = scenario
                .add_spell_to_library_top(P0, "Hidden cover", false)
                .id();
            let descriptor = TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature().properties(vec![
                        FilterProp::InTrackedSet {
                            id: TrackedSetId(0),
                        },
                    ])),
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                    ),
                ],
            };
            assert!(descriptor.names_enumerable_population());
            let effect = if typed {
                Effect::MultiplyCounter {
                    counter_type: CounterType::Plus1Plus1,
                    multiplier: 2,
                    target: descriptor,
                }
            } else {
                Effect::Double {
                    target_kind: DoubleTarget::Counters { counter_type: None },
                    target: descriptor,
                }
            };
            let sibling = AbilityDefinition::new(AbilityKind::Spell, effect)
                .target_choice_timing(TargetChoiceTiming::Resolution);
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic search and counters", false)
                .with_ability_definition(synthetic_optional_search(sibling))
                .with_mana_cost(ManaCost::generic(1))
                .id();
            fund(&mut scenario, 1, &[]);
            let mut runner = scenario.build();
            assert_library_order(&runner, &[cover, found]);
            runner.cast(spell).commit();
            let mut events = Vec::new();
            let _ = pass_to_pause(&mut runner, &mut events);
            assert_optional_prompt(&runner);
            let mut last = runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events;
            events.extend(last.clone());
            if accept {
                assert_search_prompt(&runner, &[found], true);
                last = runner
                    .act(GameAction::SelectCards { cards: vec![found] })
                    .unwrap()
                    .events;
                events.extend(last.clone());
                assert_search_reveal_and_delivery(
                    &runner,
                    &events,
                    spell,
                    Some((found, Zone::Battlefield)),
                    cover,
                    true,
                );
                assert_complete(&runner, &last);
            } else {
                assert_eq!(runner.state().objects[&found].zone, Zone::Library);
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
                assert_settled(&runner);
            }
            // CR 701.10e: each existing counter kind doubles; the typed sibling
            // changes only the named kind. Hostile objects already have counters.
            assert_eq!(
                runner.state().objects[&survivor].counters[&CounterType::Plus1Plus1],
                4
            );
            assert_eq!(
                runner.state().objects[&survivor].counters[&CounterType::Lore],
                if typed { 3 } else { 6 }
            );
            for id in [other, another, opponent] {
                assert_eq!(
                    runner.state().objects[&id].counters[&CounterType::Plus1Plus1],
                    2
                );
                assert_eq!(runner.state().objects[&id].counters[&CounterType::Lore], 3);
            }
            if accept {
                assert_eq!(
                    runner.state().objects[&found].counters[&CounterType::Plus1Plus1],
                    4
                );
                assert_eq!(
                    runner.state().objects[&found].counters[&CounterType::Lore],
                    if typed { 3 } else { 6 }
                );
            }
            assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        }
    }
}

#[test]
fn declined_search_keeps_described_host_choice() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let equipment = scenario
            .add_artifact_from_oracle(P0, "Independent Equipment", "")
            .with_subtypes(vec!["Equipment"])
            .id();
        let first = scenario.add_creature(P0, "First host", 3, 3).id();
        let second = scenario.add_creature(P0, "Second host", 3, 3).id();
        let opponent = scenario.add_creature(P1, "Opponent nonhost", 3, 3).id();
        let found = scenario
            .add_spell_to_library_top(P0, "Delivered host", false)
            .as_creature()
            .id();
        let cover = scenario
            .add_spell_to_library_top(P0, "Hidden cover", false)
            .id();
        let host = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::InTrackedSet {
                        id: TrackedSetId(0),
                    },
                ])),
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            ],
        };
        assert!(host.names_enumerable_population());
        assert!(host.denotes_battlefield_objects());
        let sibling = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Attach {
                attachment: TargetFilter::SpecificObject { id: equipment },
                target: host,
                selection: AttachSelection::Targeted,
            },
        )
        .target_choice_timing(TargetChoiceTiming::Resolution);
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic search and attachment", false)
            .with_ability_definition(synthetic_optional_search(sibling))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        assert_library_order(&runner, &[cover, found]);
        runner.cast(spell).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events,
        );
        if accept {
            assert_search_prompt(&runner, &[found], true);
            events.extend(
                runner
                    .act(GameAction::SelectCards { cards: vec![found] })
                    .unwrap()
                    .events,
            );
            assert_search_reveal_and_delivery(
                &runner,
                &events,
                spell,
                Some((found, Zone::Battlefield)),
                cover,
                true,
            );
        }
        let WaitingFor::EffectZoneChoice {
            player,
            cards,
            effect_kind,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "described host choice must survive: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*effect_kind, EffectKind::Attach);
        let mut expected = vec![first, second];
        if accept {
            expected.push(found);
        }
        let mut actual = cards.clone();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
        let last = runner
            .act(GameAction::SelectCards {
                cards: vec![second],
            })
            .unwrap()
            .events;
        assert_eq!(
            runner.state().objects[&equipment].attached_to,
            Some(AttachTarget::Object(second))
        );
        for id in [first, opponent, found] {
            assert_ne!(
                runner.state().objects[&equipment].attached_to,
                Some(AttachTarget::Object(id))
            );
        }
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&found].zone,
            if accept {
                Zone::Battlefield
            } else {
                Zone::Library
            }
        );
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert!(last.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Attach,
                ..
            }
        )));
    }
}

#[test]
fn declined_search_preserves_own_announced_generic_grant() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let chosen = scenario.add_creature(P0, "Announced recipient", 3, 3).id();
        let other = scenario
            .add_creature(P0, "Other legal recipient", 3, 3)
            .id();
        let found = scenario
            .add_spell_to_library_top(P0, "Search creature", false)
            .as_creature()
            .id();
        let cover = scenario
            .add_spell_to_library_top(P0, "Hidden cover", false)
            .id();
        let grant = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::ParentTarget)
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    }])],
                duration: Some(Duration::UntilEndOfTurn),
                end_cost: None,
            },
        );
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic search and announced grant", false)
            .with_ability_definition(synthetic_optional_search(grant))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        runner.cast(spell).target_object(chosen).commit();
        let entry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap();
        let mut announced = entry.ability().expect("cast spell has its resolved chain");
        while !matches!(announced.effect, Effect::GenericEffect { .. }) {
            announced = announced
                .sub_ability
                .as_deref()
                .expect("grant follows search/move/shuffle");
        }
        assert_eq!(announced.targets, vec![TargetRef::Object(chosen)]);
        assert_eq!(announced.target_reads, TargetReadOrigin::OwnAnnouncement);
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events,
        );
        if accept {
            assert_search_prompt(&runner, &[found], true);
            let last = runner
                .act(GameAction::SelectCards { cards: vec![found] })
                .unwrap()
                .events;
            events.extend(last.clone());
            assert_complete(&runner, &last);
            assert_search_reveal_and_delivery(
                &runner,
                &events,
                spell,
                Some((found, Zone::Battlefield)),
                cover,
                true,
            );
        } else {
            assert_settled(&runner);
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        }
        assert!(has_keyword(
            &runner.state().objects[&chosen],
            &Keyword::Flying
        ));
        for id in [other, found, cover, spell] {
            assert!(!has_keyword(&runner.state().objects[&id], &Keyword::Flying));
        }
    }
}

#[test]
fn declined_search_broadcast_binds_alternatives_exclusions_and_properties() {
    for shape in 0..5 {
        for accept in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let survivor = scenario
                .add_creature(P0, "Independent tapped recipient", 3, 3)
                .id();
            let other = scenario
                .add_creature(P0, "Independent untapped recipient", 3, 3)
                .id();
            let opponent = scenario.add_creature(P1, "Opponent creature", 3, 3).id();
            let found = scenario
                .add_spell_to_library_top(P0, "Delivered creature", false)
                .as_creature()
                .id();
            let prior = scenario
                .add_spell_to_library_top(P0, "Earlier actual reveal", false)
                .as_artifact()
                .id();
            let cover = scenario
                .add_spell_to_library_top(P0, "Hidden cover", false)
                .id();
            let absent = TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::InTrackedSet {
                    id: TrackedSetId(0),
                },
            ]));
            let creatures =
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
            let tapped = TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Tapped]),
            );
            let affected = match shape {
                0 => TargetFilter::And {
                    filters: vec![
                        creatures.clone(),
                        TargetFilter::Not {
                            filter: Box::new(absent.clone()),
                        },
                    ],
                },
                1 => TargetFilter::Or {
                    filters: vec![absent, tapped],
                },
                2 => TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::AnyOf {
                            props: vec![
                                FilterProp::InTrackedSet {
                                    id: TrackedSetId(0),
                                },
                                FilterProp::Tapped,
                            ],
                        }]),
                ),
                3 => TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Not {
                            prop: Box::new(FilterProp::InTrackedSet {
                                id: TrackedSetId(0),
                            }),
                        }]),
                ),
                4 => TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::DistinctFrom {
                            reference: Box::new(TargetFilter::ParentTarget),
                        }]),
                ),
                _ => unreachable!(),
            };
            let grant = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GenericEffect {
                    target: None,
                    static_abilities: vec![StaticDefinition::continuous()
                        .affected(affected)
                        .modifications(vec![ContinuousModification::AddKeyword {
                            keyword: Keyword::Flying,
                        }])],
                    duration: Some(Duration::UntilEndOfTurn),
                    end_cost: None,
                },
            );
            let mut search = synthetic_optional_search(grant);
            search.sub_link = SubAbilityLink::SequentialSibling;
            let root = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Reveal {
                    target: TargetFilter::SpecificObject { id: prior },
                },
            )
            .sub_ability(search);
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic reveal search and broadcast", false)
                .with_ability_definition(root)
                .with_mana_cost(ManaCost::generic(1))
                .id();
            fund(&mut scenario, 1, &[]);
            let mut runner = scenario.build();
            runner
                .state_mut()
                .objects
                .get_mut(&survivor)
                .unwrap()
                .tapped = true;
            assert_library_order(&runner, &[cover, prior, found]);
            runner.cast(spell).commit();
            let mut events = Vec::new();
            let _ = pass_to_pause(&mut runner, &mut events);
            assert_optional_prompt(&runner);
            assert_eq!(runner.state().last_revealed_ids, vec![prior]);
            events.extend(
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .unwrap()
                    .events,
            );
            if accept {
                assert_search_prompt(&runner, &[found], true);
                let last = runner
                    .act(GameAction::SelectCards { cards: vec![found] })
                    .unwrap()
                    .events;
                events.extend(last.clone());
                assert_complete(&runner, &last);
                assert_eq!(runner.state().objects[&found].zone, Zone::Battlefield);
            } else {
                assert_settled(&runner);
                assert_eq!(runner.state().objects[&found].zone, Zone::Library);
            }
            assert!(has_keyword(
                &runner.state().objects[&survivor],
                &Keyword::Flying
            ));
            assert_eq!(
                has_keyword(&runner.state().objects[&other], &Keyword::Flying),
                !matches!(shape, 1 | 2)
            );
            assert_eq!(
                has_keyword(&runner.state().objects[&found], &Keyword::Flying),
                accept && matches!(shape, 1 | 2)
            );
            for id in [opponent, prior, cover, spell] {
                assert!(!has_keyword(&runner.state().objects[&id], &Keyword::Flying));
            }
            let reveals: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                reveals,
                if accept {
                    vec![vec![prior], vec![found]]
                } else {
                    vec![vec![prior]]
                }
            );
        }
    }
}

#[test]
fn declined_search_impossible_counter_unit_keeps_later_player_work() {
    for typed in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creature = scenario
            .add_creature(P0, "Hostile existing creature", 3, 3)
            .id();
        scenario.with_counter(creature, CounterType::Plus1Plus1, 2);
        let found = scenario
            .add_spell_to_library_top(P0, "Unchosen creature", false)
            .as_creature()
            .id();
        let target = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::InTrackedSet {
                id: TrackedSetId(0),
            },
        ]));
        let effect = if typed {
            Effect::MultiplyCounter {
                counter_type: CounterType::Plus1Plus1,
                multiplier: 2,
                target,
            }
        } else {
            Effect::Double {
                target_kind: DoubleTarget::Counters { counter_type: None },
                target,
            }
        };
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let sibling = AbilityDefinition::new(AbilityKind::Spell, effect)
            .target_choice_timing(TargetChoiceTiming::Resolution)
            .sub_ability(life);
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic impossible population", false)
            .with_ability_definition(synthetic_optional_search(sibling))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        runner.cast(spell).commit();
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(
            runner.state().objects[&creature].counters[&CounterType::Plus1Plus1],
            2
        );
        assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_keeps_chosen_counter_target_and_player_double_roles() {
    for target_kind in [
        DoubleTarget::Counters { counter_type: None },
        DoubleTarget::LifeTotal,
        DoubleTarget::ManaPool { color: None },
    ] {
        let counters = matches!(target_kind, DoubleTarget::Counters { .. });
        let life = matches!(target_kind, DoubleTarget::LifeTotal);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let chosen = scenario
            .add_creature(P0, "Chosen counter recipient", 3, 3)
            .id();
        let other = scenario
            .add_creature(P0, "Other counter recipient", 3, 3)
            .id();
        scenario
            .with_counter(chosen, CounterType::Plus1Plus1, 2)
            .with_counter(other, CounterType::Plus1Plus1, 2);
        let found = scenario
            .add_spell_to_library_top(P0, "Unchosen search creature", false)
            .as_creature()
            .id();
        let target = if counters {
            TargetFilter::Typed(TypedFilter::creature())
        } else {
            TargetFilter::Controller
        };
        let sibling = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Double {
                target_kind,
                target,
            },
        );
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic preserved double role", false)
            .with_ability_definition(synthetic_optional_search(sibling))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[ManaType::Blue]);
        let mut runner = scenario.build();
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Colorless),
            1
        );
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Blue),
            1
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 2);
        if counters {
            runner.cast(spell).target_object(chosen).commit();
        } else {
            runner.cast(spell).commit();
        }
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Colorless),
            0
        );
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Blue),
            1
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(
            runner.state().objects[&spell].mana_cost,
            ManaCost::generic(1)
        );
        let paid = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap();
        assert!(matches!(
            paid.kind,
            StackEntryKind::Spell {
                actual_mana_spent: 1,
                ..
            }
        ));
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Colorless),
            0
        );
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Blue),
            1
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
        let decision = runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Colorless),
            0
        );
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Blue),
            if counters || life { 1 } else { 2 }
        );
        let added: Vec<_> = decision
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaAdded { .. }))
            .collect();
        if counters || life {
            assert!(added.is_empty());
        } else {
            assert!(
                matches!(added.as_slice(), [GameEvent::ManaAdded { player_id: P0, mana_type: ManaType::Blue, source_id, tap_state: ManaTapState::NotFromTap }] if *source_id == spell)
            );
        }
        assert_eq!(
            runner.state().objects[&chosen].counters[&CounterType::Plus1Plus1],
            if counters { 4 } else { 2 }
        );
        assert_eq!(
            runner.state().objects[&other].counters[&CounterType::Plus1Plus1],
            2
        );
        assert_eq!(runner.state().players[0].life, if life { 40 } else { 20 });
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            if counters || life { 1 } else { 2 }
        );
        assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_empty_described_host_keeps_later_player_work() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Independent Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let host = scenario
        .add_creature(P0, "Existing nonrecipient", 3, 3)
        .id();
    let found = scenario
        .add_spell_to_library_top(P0, "Unchosen creature", false)
        .as_creature()
        .id();
    let mut life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );
    life.sub_link = SubAbilityLink::SequentialSibling;
    let sibling = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Attach {
            attachment: TargetFilter::SpecificObject { id: equipment },
            target: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::InTrackedSet {
                    id: TrackedSetId(0),
                },
            ])),
            selection: AttachSelection::Targeted,
        },
    )
    .target_choice_timing(TargetChoiceTiming::Resolution)
    .sub_ability(life);
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic empty described host", false)
        .with_ability_definition(synthetic_optional_search(sibling))
        .with_mana_cost(ManaCost::generic(1))
        .id();
    fund(&mut scenario, 1, &[]);
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    let mut events = Vec::new();
    let _ = pass_to_pause(&mut runner, &mut events);
    assert_optional_prompt(&runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_eq!(runner.state().players[0].life, 23);
    assert_eq!(runner.state().objects[&equipment].attached_to, None);
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&found].zone, Zone::Library);
    assert_settled(&runner);
}

// Synthetic API contracts for the residual classifier, not printed-card claims.
#[test]
fn declined_search_own_announced_counter_preserves_secondary_parent_authority() {
    for operand in 0..5 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let chosen = scenario
            .add_creature(P0, "Same-name recipient", 3, 3)
            .with_mana_cost(ManaCost::generic(2))
            .with_color(vec![ManaColor::Blue])
            .id();
        let twin = scenario
            .add_creature(P0, "Same-name recipient", 3, 3)
            .with_color(vec![ManaColor::Red])
            .id();
        let other = scenario.add_creature(P0, "Different creature", 3, 3).id();
        let found = scenario
            .add_spell_to_library_top(P0, "Unchosen creature", false)
            .as_creature()
            .id();
        let same_name = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::SameNameAsParentTarget]),
        );
        let count = match operand {
            0 => QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter: same_name },
            },
            1 => QuantityExpr::Ref {
                qty: QuantityRef::TargetObjectManaValue {
                    filter: Box::new(TargetFilter::ParentTarget),
                },
            },
            2 => QuantityExpr::Ref {
                qty: QuantityRef::DistinctColorsAmong {
                    source: CardTypeSetSource::Objects { filter: same_name },
                },
            },
            3 => QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            },
            4 => QuantityExpr::Fixed { value: 1 },
            _ => unreachable!(),
        };
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let mut counter = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count,
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(life);
        if operand == 4 {
            counter.repeat_for = Some(QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            });
        }
        let spell = scenario
            .add_spell_to_hand(P0, "Distinct search source", false)
            .with_ability_definition(synthetic_optional_search(counter))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        runner.cast(spell).target_object(chosen).commit();
        assert!(runner.state().players[0].mana_pool.is_empty());
        let entry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap();
        let mut announced = entry.ability().expect("cast spell has its resolved chain");
        while !matches!(announced.effect, Effect::PutCounter { .. }) {
            announced = announced
                .sub_ability
                .as_deref()
                .expect("counter follows search");
        }
        // CR 601.2c + CR 400.7: public announcement records the chosen
        // occurrence and its actual incarnation, rather than seeded targets.
        assert_eq!(announced.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert_eq!(announced.targets, vec![TargetRef::Object(chosen)]);
        assert_eq!(
            announced.target_pins,
            vec![Some(ObjectIncarnationRef::from_object(
                &runner.state().objects[&chosen]
            ))]
        );
        let mut events = Vec::new();
        let _ = pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        // CR 608.2c + CR 608.2h: secondary name/population information
        // reads the instruction's own chosen object; missing current-set
        // scalars and repeats still remove only that dependent instruction.
        assert_eq!(
            runner.state().objects[&chosen]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            if operand < 3 { 2 } else { 0 }
        );
        for id in [twin, other, found, spell] {
            assert!(!runner.state().objects[&id]
                .counters
                .contains_key(&CounterType::Plus1Plus1));
        }
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(runner.state().objects[&found].zone, Zone::Library);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_refuses_unavailable_nested_names_and_quantity_carriers() {
    for origin in [
        TargetReadOrigin::OwnAnnouncement,
        TargetReadOrigin::ParentAnnouncement,
    ] {
        for carrier in 0..5 {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_creature(P0, "Named independent creature", 3, 3);
            scenario.add_creature(P0, "Named independent creature", 3, 3);
            scenario.add_creature(P0, "Different creature", 3, 3);
            let first_hand = scenario
                .add_spell_to_hand(P0, "First hand card", false)
                .id();
            let second_hand = scenario
                .add_spell_to_hand(P0, "Second hand card", false)
                .id();
            let found = scenario
                .add_spell_to_library_top(P0, "Unchosen creature", false)
                .as_creature()
                .id();
            let property = match carrier {
                0 => FilterProp::SameNameAsParentTarget,
                1 | 3 => FilterProp::Not {
                    prop: Box::new(FilterProp::SameNameAsParentTarget),
                },
                2 => FilterProp::AnyOf {
                    props: vec![FilterProp::Not {
                        prop: Box::new(FilterProp::SameNameAsParentTarget),
                    }],
                },
                4 => FilterProp::Named {
                    name: "Named independent creature".to_owned(),
                },
                _ => unreachable!(),
            };
            let filter = TargetFilter::Typed(if carrier == 3 {
                TypedFilter::card().properties(vec![property])
            } else {
                TypedFilter::creature().properties(vec![property])
            });
            let quantity = if carrier == 3 {
                QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Hand,
                    card_types: vec![],
                    filter: Some(filter),
                    scope: CountScope::Controller,
                }
            } else {
                QuantityRef::ObjectCount { filter }
            };
            let mut life = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            );
            life.sub_link = SubAbilityLink::SequentialSibling;
            let mut counted_life = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Ref { qty: quantity },
                    player: TargetFilter::Controller,
                },
            )
            .sub_ability(life);
            counted_life.target_reads = origin;
            let spell = scenario
                .add_spell_to_hand(P0, "Distinct search source", false)
                .with_ability_definition(synthetic_optional_search(counted_life))
                .with_mana_cost(ManaCost::generic(1))
                .id();
            fund(&mut scenario, 1, &[]);
            let mut runner = scenario.build();
            runner.cast(spell).commit();
            assert!(runner.state().players[0].mana_pool.is_empty());
            let entry = runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.source_id == spell)
                .unwrap();
            let mut announced = entry.ability().unwrap();
            while !matches!(announced.effect, Effect::GainLife { .. }) {
                announced = announced.sub_ability.as_deref().unwrap();
            }
            assert_eq!(announced.target_reads, origin);
            assert!(announced.targets.is_empty());
            assert!(announced.target_pins.is_empty());
            assert_eq!(runner.state().players[0].hand.len(), 2);
            let mut events = Vec::new();
            let _ = pass_to_pause(&mut runner, &mut events);
            assert_optional_prompt(&runner);
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap();
            // CR 608.2c + CR 608.2h: no own declaration or proved parent
            // announcement supplies this name. The fixed sibling still runs,
            // while an independently specified name remains a live count.
            assert_eq!(
                runner.state().players[0].life,
                if carrier == 4 { 25 } else { 23 }
            );
            for id in [first_hand, second_hand] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
            }
            assert_eq!(runner.state().objects[&found].zone, Zone::Library);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_settled(&runner);
        }
    }
}

#[test]
fn declined_search_refuses_unavailable_nested_scalar_thresholds() {
    for origin in [
        TargetReadOrigin::OwnAnnouncement,
        TargetReadOrigin::ParentAnnouncement,
    ] {
        for threshold in [
            QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::TargetControllerCounter {
                    kind: PlayerCounterKind::Poison,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::NameStickerLetterCount {
                    stickers: NameStickerSet::OnObject {
                        scope: ObjectScope::Target,
                    },
                    letters: LetterQuery::Letter { letter: 'o' },
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::Target,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::ParentObjectTarget {
                        possession: PossessionAxis::Controller,
                    },
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::Total,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::DistinctColors,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::OfColor {
                        color: ManaColor::Green,
                    },
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::FromSource {
                        source_filter: TargetFilter::Typed(TypedFilter::land()),
                    },
                },
            },
            QuantityExpr::Fixed { value: 0 },
        ] {
            let independent = matches!(threshold, QuantityExpr::Fixed { .. });
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let hand = [
                scenario.add_spell_to_hand(P0, "First zero", false).id(),
                scenario.add_spell_to_hand(P0, "Second zero", false).id(),
            ];
            let found = scenario
                .add_spell_to_library_top(P0, "Search candidate", false)
                .as_creature()
                .id();
            let cover = scenario
                .add_spell_to_library_top(P0, "Cover above candidate", false)
                .id();
            let mut life = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            );
            life.sub_link = SubAbilityLink::SequentialSibling;
            let mut counted = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Hand,
                            scope: CountScope::Controller,
                            card_types: vec![],
                            filter: Some(TargetFilter::Typed(TypedFilter::card().properties(
                                vec![FilterProp::Cmc {
                                    comparator: Comparator::EQ,
                                    value: threshold,
                                }],
                            ))),
                        },
                    },
                    player: TargetFilter::Controller,
                },
            )
            .sub_ability(life);
            counted.target_reads = origin;
            let spell = scenario
                .add_spell_to_hand(P0, "Paid nested scalar", false)
                .with_ability_definition(synthetic_optional_search(counted))
                .with_mana_cost(ManaCost::generic(1))
                .id();
            fund(&mut scenario, 1, &[]);
            let mut runner = scenario.build();
            runner.cast(spell).commit();
            assert!(runner.state().players[0].mana_pool.is_empty());
            let root = runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.source_id == spell)
                .unwrap()
                .ability()
                .unwrap();
            let counted = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
                .find(|node| matches!(node.effect, Effect::GainLife { .. }))
                .unwrap();
            assert_eq!(counted.target_reads, origin);
            assert!(counted.targets.is_empty() && counted.target_pins.is_empty());
            assert!(counted.effect_context_object.is_none());
            assert_eq!(runner.state().players[0].hand.len(), 2);
            for id in hand {
                assert_eq!(runner.state().objects[&id].mana_cost.mana_value(), 0);
            }
            let mut events = Vec::new();
            pass_to_pause(&mut runner, &mut events);
            assert_optional_prompt(&runner);
            let parked = runner
                .state()
                .active_optional_effect_frame()
                .expect("the prompt owns the actual optional instruction")
                .ability
                .as_ref();
            let current = std::iter::successors(Some(parked), |node| node.sub_ability.as_deref())
                .find(|node| matches!(node.effect, Effect::GainLife { .. }))
                .unwrap();
            assert_eq!(current.target_reads, origin);
            assert!(current.targets.is_empty() && current.target_pins.is_empty());
            assert!(current.effect_context_object.is_none());
            assert_eq!(runner.state().players[0].hand.len(), 2);
            assert!(runner.state().last_revealed_ids.is_empty());
            assert!(runner.state().players[0].mana_pool.is_empty());
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap();
            assert_eq!(
                runner.state().players[0].life,
                if independent { 25 } else { 23 }
            );
            assert_eq!(runner.state().players[1].life, 20);
            assert_eq!(runner.state().players[0].hand.len(), 2);
            for id in hand {
                assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
            }
            for id in [found, cover] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Library);
            }
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_settled(&runner);
        }
    }
}

#[test]
fn declined_search_owned_threshold_keeps_only_available_object_authority() {
    for parameter in 0..5 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut chosen_builder = scenario.add_creature(P0, "Chosen MV two", 3, 3);
        chosen_builder.with_mana_cost(ManaCost::generic(2));
        if parameter == 2 || parameter == 4 {
            chosen_builder.controlled_by(P1);
        }
        let chosen = chosen_builder.id();
        let other = scenario
            .add_creature(P0, "Other MV five", 3, 3)
            .with_mana_cost(ManaCost::generic(5))
            .id();
        let hand = [
            scenario
                .add_spell_to_hand(P0, "First hand", false)
                .with_mana_cost(ManaCost::generic(if parameter == 1 {
                    0
                } else if parameter == 4 {
                    1
                } else {
                    2
                }))
                .id(),
            scenario
                .add_spell_to_hand(P0, "Second hand", false)
                .with_mana_cost(ManaCost::generic(if parameter == 1 {
                    0
                } else if parameter == 4 {
                    1
                } else {
                    2
                }))
                .id(),
        ];
        if parameter == 4 {
            scenario.add_spell_to_hand(P1, "Opponent hand", false);
        }
        let found = scenario
            .add_spell_to_library_top(P0, "Candidate", false)
            .as_creature()
            .id();
        let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
        let threshold = match parameter {
            0 => QuantityRef::ObjectManaValue {
                scope: ObjectScope::Target,
            },
            1 => QuantityRef::TrackedSetSize,
            2 => QuantityRef::TargetControllerCounter {
                kind: PlayerCounterKind::Poison,
            },
            3 => QuantityRef::NameStickerLetterCount {
                stickers: NameStickerSet::OnObject {
                    scope: ObjectScope::Target,
                },
                letters: LetterQuery::Letter { letter: 'o' },
            },
            4 => QuantityRef::HandSize {
                player: PlayerScope::RecipientController,
            },
            _ => unreachable!(),
        };
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let counter = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        scope: CountScope::Controller,
                        card_types: vec![],
                        filter: Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                            FilterProp::Cmc {
                                comparator: Comparator::EQ,
                                value: QuantityExpr::Ref { qty: threshold },
                            },
                        ]))),
                    },
                },
            },
        )
        .sub_ability(life);
        let spell = scenario
            .add_spell_to_hand(P0, "Paid own threshold", false)
            .with_ability_definition(synthetic_optional_search(counter))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&chosen].owner, P0);
        assert_eq!(
            runner.state().objects[&chosen].controller,
            if parameter == 2 || parameter == 4 {
                P1
            } else {
                P0
            }
        );
        if parameter == 3 {
            runner
                .state_mut()
                .objects
                .get_mut(&chosen)
                .unwrap()
                .stickers
                .push(AppliedSticker::Name {
                    locator: StickerLocator {
                        sheet: "Fixture".to_string(),
                        index: 0,
                    },
                    text: "Hot Dog".to_string(),
                    position: 0,
                    timestamp: 0,
                });
        }
        runner.state_mut().players[0].poison_counters = 1;
        runner.state_mut().players[1].poison_counters = 2;
        runner.cast(spell).target_object(chosen).commit();
        assert_eq!(
            runner.state().objects[&chosen].controller,
            if parameter == 2 || parameter == 4 {
                P1
            } else {
                P0
            }
        );
        let root = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap()
            .ability()
            .unwrap();
        let counter = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::PutCounter { .. }))
            .unwrap();
        assert_eq!(counter.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert_eq!(counter.targets, vec![TargetRef::Object(chosen)]);
        assert_eq!(
            counter.target_pins,
            vec![Some(ObjectIncarnationRef::from_object(
                &runner.state().objects[&chosen]
            ))]
        );
        assert!(
            matches!(&counter.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots == &[0])
        );
        assert_eq!(runner.state().objects[&chosen].owner, P0);
        assert_eq!(runner.state().players[0].hand.len(), 2);
        assert!(runner.state().players[0].mana_pool.is_empty());
        let mut events = Vec::new();
        pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        assert_eq!(runner.state().objects[&chosen].owner, P0);
        assert_eq!(
            runner.state().objects[&chosen].controller,
            if parameter == 2 || parameter == 4 {
                P1
            } else {
                P0
            }
        );
        let parked = runner
            .state()
            .active_optional_effect_frame()
            .expect("the prompt owns the actual optional instruction")
            .ability
            .as_ref();
        let current = std::iter::successors(Some(parked), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::PutCounter { .. }))
            .unwrap();
        assert_eq!(current.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert_eq!(current.targets, vec![TargetRef::Object(chosen)]);
        assert_eq!(
            current.target_pins,
            vec![Some(ObjectIncarnationRef::from_object(
                &runner.state().objects[&chosen]
            ))]
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(
            runner.state().objects[&chosen]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            if parameter == 1 { 0 } else { 2 }
        );
        for id in [other, found, cover, spell] {
            assert!(!runner.state().objects[&id]
                .counters
                .contains_key(&CounterType::Plus1Plus1));
        }
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(runner.state().players[1].life, 20);
        assert_eq!(runner.state().players[0].hand.len(), 2);
        for id in hand {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
        }
        for id in [found, cover] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_player_matching_keeps_announced_recipient() {
    for parameter in 0..5 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, "Caster creature", 2, 2);
        scenario.add_creature(P1, "Opponent creature", 2, 2);
        let hand = [
            scenario
                .add_spell_to_hand(P0, "First MV two", false)
                .with_mana_cost(ManaCost::generic(if parameter >= 3 { 1 } else { 2 }))
                .id(),
            scenario
                .add_spell_to_hand(P0, "Second MV two", false)
                .with_mana_cost(ManaCost::generic(if parameter >= 3 { 1 } else { 2 }))
                .id(),
        ];
        if parameter >= 3 {
            scenario.add_spell_to_hand(P1, "Opponent hand", false);
        }
        let found = scenario
            .add_spell_to_library_top(P0, "Candidate", false)
            .as_creature()
            .id();
        let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
        let amount = if parameter >= 2 {
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Hand,
                    scope: CountScope::Controller,
                    card_types: vec![],
                    filter: Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::Cmc {
                            comparator: Comparator::EQ,
                            value: QuantityExpr::Ref {
                                qty: match parameter {
                                    2 => QuantityRef::TargetControllerCounter {
                                        kind: PlayerCounterKind::Poison,
                                    },
                                    3 => QuantityRef::HandSize {
                                        player: PlayerScope::Target,
                                    },
                                    4 => QuantityRef::HandSize {
                                        player: PlayerScope::ParentObjectTarget {
                                            possession: PossessionAxis::Controller,
                                        },
                                    },
                                    _ => unreachable!(),
                                },
                            },
                        },
                    ]))),
                },
            }
        } else {
            QuantityExpr::Fixed { value: 2 }
        };
        let player = TargetFilter::PlayerMatching {
            player: Box::new(PlayerFilter::ControlsCount {
                relation: PlayerRelation::All,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                comparator: Comparator::GE,
                count: Box::new(if parameter == 0 {
                    QuantityExpr::Ref {
                        qty: QuantityRef::TrackedSetSize,
                    }
                } else {
                    QuantityExpr::Fixed { value: 0 }
                }),
            }),
        };
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let counted =
            AbilityDefinition::new(AbilityKind::Spell, Effect::GainLife { amount, player })
                .sub_ability(life);
        let spell = scenario
            .add_spell_to_hand(P0, "Paid player recipient", false)
            .with_ability_definition(synthetic_optional_search(counted))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        runner.state_mut().players[0].poison_counters = 1;
        runner.state_mut().players[1].poison_counters = 2;
        runner.cast(spell).target_player(P1).commit();
        let root = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap()
            .ability()
            .unwrap();
        let counted = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::GainLife { .. }))
            .unwrap();
        assert_eq!(counted.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert_eq!(counted.targets, vec![TargetRef::Player(P1)]);
        assert!(counted.target_pins.iter().all(Option::is_none));
        assert!(
            matches!(&counted.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots == &[0])
        );
        assert!(counted.effect_context_object.is_none());
        assert_eq!(runner.state().players[0].hand.len(), 2);
        for id in hand {
            assert_eq!(
                runner.state().objects[&id].mana_cost.mana_value(),
                if parameter >= 3 { 1 } else { 2 }
            );
        }
        assert!(runner.state().players[0].mana_pool.is_empty());
        let mut events = Vec::new();
        pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        let parked = runner
            .state()
            .active_optional_effect_frame()
            .expect("the prompt owns the actual optional instruction")
            .ability
            .as_ref();
        let current = std::iter::successors(Some(parked), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::GainLife { .. }))
            .unwrap();
        assert_eq!(current.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert_eq!(current.targets, vec![TargetRef::Player(P1)]);
        assert!(current.target_pins.iter().all(Option::is_none));
        assert!(current.effect_context_object.is_none());
        assert_eq!(runner.state().players[0].poison_counters, 1);
        assert_eq!(runner.state().players[1].poison_counters, 2);
        assert_eq!(runner.state().players[0].hand.len(), 2);
        assert_eq!(
            runner.state().players[1].hand.len(),
            usize::from(parameter >= 3)
        );
        assert!(runner.state().last_revealed_ids.is_empty());
        let decision = runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(
            runner.state().players[1].life,
            if parameter == 0 { 20 } else { 22 }
        );
        for id in hand {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
        }
        for id in [found, cover] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(decision.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::GainLife,
                ..
            }
        )));
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_player_declaration_does_not_supply_object_information() {
    for dynamic in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P0, "First creature", 2, 2).id();
        let second = scenario.add_creature(P0, "Second creature", 2, 2).id();
        let third = scenario.add_creature(P1, "Third creature", 2, 2).id();
        let found = scenario
            .add_spell_to_library_top(P0, "Candidate", false)
            .as_creature()
            .id();
        let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
        let amount = if dynamic {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                        FilterProp::Not {
                            prop: Box::new(FilterProp::SameNameAsParentTarget),
                        },
                    ])),
                },
            }
        } else {
            QuantityExpr::Fixed { value: 3 }
        };
        let player = TargetFilter::PlayerMatching {
            player: Box::new(PlayerFilter::ControlsCount {
                relation: PlayerRelation::All,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                comparator: Comparator::GE,
                count: Box::new(QuantityExpr::Fixed { value: 0 }),
            }),
        };
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let dependent =
            AbilityDefinition::new(AbilityKind::Spell, Effect::GainLife { amount, player })
                .sub_ability(life);
        let spell = scenario
            .add_spell_to_hand(P0, "Paid player object role", false)
            .with_ability_definition(synthetic_optional_search(dependent))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        runner.cast(spell).target_player(P1).commit();
        let root = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap()
            .ability()
            .unwrap();
        let node = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::GainLife { .. }))
            .unwrap();
        assert_eq!(node.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert_eq!(node.targets, vec![TargetRef::Player(P1)]);
        assert!(node.target_pins.iter().all(Option::is_none));
        assert!(
            matches!(&node.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots == &[0])
        );
        assert!(runner.state().players[0].mana_pool.is_empty());
        let mut events = Vec::new();
        pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(
            runner.state().players[1].life,
            if dynamic { 20 } else { 23 }
        );
        for id in [first, second, third] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        for id in [found, cover] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_scalar_player_fields_keep_candidate_authority() {
    for parameter in 0..10 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, "Caster population", 2, 2);
        scenario.add_creature(P1, "Opponent population", 2, 2);
        let hand = [
            scenario.add_spell_to_hand(P0, "First hand", false).id(),
            scenario.add_spell_to_hand(P0, "Second hand", false).id(),
        ];
        let opponent_hand = scenario.add_spell_to_hand(P1, "Opponent hand", false).id();
        let found = scenario
            .add_spell_to_library_top(P0, "Candidate", false)
            .as_creature()
            .id();
        let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
        let missing = QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        };
        let player = match parameter {
            0 | 1 => PlayerFilter::ControlsCount {
                relation: PlayerRelation::All,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                comparator: Comparator::GE,
                count: Box::new(if parameter == 0 {
                    missing
                } else {
                    QuantityExpr::Fixed { value: 0 }
                }),
            },
            2 | 3 => PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::All,
                attr: Box::new(QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                }),
                comparator: Comparator::GE,
                value: Box::new(if parameter == 2 {
                    missing
                } else {
                    QuantityExpr::Fixed { value: 20 }
                }),
            },
            4..=9 => PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::All,
                attr: Box::new(QuantityRef::HandSize {
                    player: match parameter {
                        6 | 7 => PlayerScope::ParentObjectTarget {
                            possession: PossessionAxis::Controller,
                        },
                        8 | 9 => PlayerScope::AllPlayers {
                            aggregate: AggregateFunction::Sum,
                            exclude: Some(Box::new(PlayerScope::ParentObjectTarget {
                                possession: PossessionAxis::Controller,
                            })),
                        },
                        _ => PlayerScope::Controller,
                    },
                }),
                comparator: Comparator::EQ,
                value: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: if parameter % 2 == 0 {
                            PlayerScope::ScopedPlayer
                        } else {
                            PlayerScope::Controller
                        },
                    },
                }),
            },
            _ => unreachable!(),
        };
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let counted = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount { filter: player },
                },
                player: TargetFilter::Controller,
            },
        )
        .sub_ability(life);
        let spell = scenario
            .add_spell_to_hand(P0, "Paid candidate scope", false)
            .with_ability_definition(synthetic_optional_search(counted))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        runner.cast(spell).commit();
        assert_eq!(runner.state().players[0].hand.len(), 2);
        assert_eq!(runner.state().players[1].hand.len(), 1);
        assert!(runner.state().players[0].mana_pool.is_empty());
        let mut events = Vec::new();
        pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(
            runner.state().players[0].life,
            match parameter {
                0 | 2 => 23,
                5 | 7 | 9 => 24,
                _ => 25,
            }
        );
        assert_eq!(runner.state().players[1].life, 20);
        for id in hand.into_iter().chain([opponent_hand]) {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
        }
        for id in [found, cover] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_raw_speed_scope_refuses_unavailable_threshold() {
    for missing in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creatures = [
            scenario.add_creature(P0, "Caster population", 2, 2).id(),
            scenario.add_creature(P1, "Opponent population", 2, 2).id(),
        ];
        let found = scenario
            .add_spell_to_library_top(P0, "Candidate", false)
            .as_creature()
            .id();
        let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let speed = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::StartYourEngines {
                player_scope: PlayerFilter::ControlsCount {
                    relation: PlayerRelation::All,
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                    comparator: Comparator::GE,
                    count: Box::new(if missing {
                        QuantityExpr::Ref {
                            qty: QuantityRef::TrackedSetSize,
                        }
                    } else {
                        QuantityExpr::Fixed { value: 0 }
                    }),
                },
            },
        )
        .sub_ability(life);
        let spell = scenario
            .add_spell_to_hand(P0, "Paid raw speed scope", false)
            .with_ability_definition(synthetic_optional_search(speed))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        fund(&mut scenario, 1, &[]);
        let mut runner = scenario.build();
        assert!(runner
            .state()
            .players
            .iter()
            .all(|player| player.speed.is_none()));
        runner.cast(spell).commit();
        assert!(runner.state().players[0].mana_pool.is_empty());
        let mut events = Vec::new();
        pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        for id in creatures {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        assert!(runner
            .state()
            .players
            .iter()
            .all(|player| player.speed.is_none()));
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(
            runner
                .state()
                .players
                .iter()
                .map(|player| player.speed)
                .collect::<Vec<_>>(),
            vec![if missing { None } else { Some(1) }; 2]
        );
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(runner.state().players[1].life, 20);
        for id in [found, cover] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}

#[test]
fn declined_search_keeps_exact_prior_declared_object_slot() {
    for object_first in [false, true] {
        for index in 0..3 {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let chosen = scenario.add_creature(P0, "Chosen prior object", 3, 3).id();
            let other = scenario.add_creature(P0, "Other legal object", 3, 3).id();
            let found = scenario
                .add_spell_to_library_top(P0, "Candidate", false)
                .as_creature()
                .id();
            let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
            let mut life = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            );
            life.sub_link = SubAbilityLink::SequentialSibling;
            let mut absent = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                },
            )
            .sub_ability(life);
            absent.sub_link = SubAbilityLink::SequentialSibling;
            let mut indexed = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::ParentTargetSlot { index },
                },
            )
            .sub_ability(absent);
            indexed.target_reads = TargetReadOrigin::ParentAnnouncement;
            let mut search = synthetic_optional_search(indexed);
            search.sub_link = SubAbilityLink::SequentialSibling;
            let mut player = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::PlayerMatching {
                        player: Box::new(PlayerFilter::All),
                    },
                },
            );
            let mut counter = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            );
            player.sub_link = SubAbilityLink::SequentialSibling;
            counter.sub_link = SubAbilityLink::SequentialSibling;
            let root = if object_first {
                counter.sub_ability(player.sub_ability(search))
            } else {
                player.sub_ability(counter.sub_ability(search))
            };
            let spell = scenario
                .add_spell_to_hand(P0, "Paid exact prior slot", false)
                .with_ability_definition(root)
                .with_mana_cost(ManaCost::generic(1))
                .id();
            fund(&mut scenario, 1, &[]);
            let mut runner = scenario.build();
            runner
                .cast(spell)
                .target_player(P1)
                .target_object(chosen)
                .commit();
            let root = runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.source_id == spell)
                .unwrap()
                .ability()
                .unwrap();
            let nodes: Vec<_> =
                std::iter::successors(Some(root), |node| node.sub_ability.as_deref()).collect();
            let declared_counter = nodes
                .iter()
                .find(|node| {
                    matches!(
                        node.effect,
                        Effect::PutCounter {
                            target: TargetFilter::Typed(_),
                            ..
                        }
                    )
                })
                .unwrap();
            assert_eq!(declared_counter.targets, vec![TargetRef::Object(chosen)]);
            assert_eq!(
                declared_counter.target_pins,
                vec![Some(ObjectIncarnationRef::from_object(
                    &runner.state().objects[&chosen]
                ))]
            );
            let indexed = nodes
                .iter()
                .find(|node| {
                    matches!(
                        node.effect,
                        Effect::PutCounter {
                            target: TargetFilter::ParentTargetSlot { .. },
                            ..
                        }
                    )
                })
                .unwrap();
            assert_eq!(indexed.target_reads, TargetReadOrigin::ParentAnnouncement);
            assert!(indexed.chosen_target_slots.slots().is_empty());
            assert!(runner.state().players[0].mana_pool.is_empty());
            let mut events = Vec::new();
            pass_to_pause(&mut runner, &mut events);
            assert_optional_prompt(&runner);
            assert_eq!(runner.state().players[1].life, 21);
            assert_eq!(
                runner.state().objects[&chosen].counters[&CounterType::Plus1Plus1],
                1
            );
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap();
            let object_index = usize::from(!object_first);
            assert_eq!(
                runner.state().objects[&chosen].counters[&CounterType::Plus1Plus1],
                if index == object_index { 3 } else { 1 }
            );
            assert!(!runner.state().objects[&other]
                .counters
                .contains_key(&CounterType::Plus1Plus1));
            assert_eq!(runner.state().players[0].life, 23);
            assert_eq!(runner.state().players[1].life, 21);
            for id in [found, cover] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Library);
            }
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_settled(&runner);
        }
    }
}

// CR 608.2c + CR 608.2h: inherited Player information is read when its retained instruction applies.
#[test]
fn declined_search_inherited_player_scalar_uses_each_runtime_entry() {
    for prior in [false, true] {
        for shape in 0..3 {
            for parameter in 0..3 {
                if prior && parameter != 2 {
                    continue;
                }
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                scenario.add_creature(P0, "Caster population", 2, 2);
                scenario.add_creature(P1, "Opponent population", 2, 2);
                let mv = if parameter == 2 { 2 } else { 1 };
                let hand = [
                    scenario
                        .add_spell_to_hand(P0, "First matching hand", false)
                        .with_mana_cost(ManaCost::generic(mv))
                        .id(),
                    scenario
                        .add_spell_to_hand(P0, "Second matching hand", false)
                        .with_mana_cost(ManaCost::generic(mv))
                        .id(),
                ];
                let opponent_hand = scenario.add_spell_to_hand(P1, "Opponent hand", false).id();
                let found = scenario
                    .add_spell_to_library_top(P0, "Search candidate", false)
                    .as_creature()
                    .id();
                let cover = scenario
                    .add_spell_to_library_top(P0, "Hidden cover", false)
                    .id();
                let threshold = match parameter {
                    0 => QuantityRef::HandSize {
                        player: PlayerScope::Target,
                    },
                    1 => QuantityRef::HandSize {
                        player: PlayerScope::ParentObjectTarget {
                            possession: PossessionAxis::Controller,
                        },
                    },
                    2 => QuantityRef::TargetControllerCounter {
                        kind: PlayerCounterKind::Poison,
                    },
                    _ => unreachable!(),
                };
                let mut life = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                        player: TargetFilter::Controller,
                    },
                );
                life.sub_link = SubAbilityLink::SequentialSibling;
                let mut amount = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::ZoneCardCount {
                                zone: ZoneRef::Hand,
                                scope: CountScope::Controller,
                                card_types: vec![],
                                filter: Some(TargetFilter::Typed(TypedFilter::card().properties(
                                    vec![FilterProp::Cmc {
                                        comparator: Comparator::EQ,
                                        value: QuantityExpr::Ref { qty: threshold },
                                    }],
                                ))),
                            },
                        },
                        player: TargetFilter::ParentTarget,
                    },
                )
                .sub_ability(life);
                amount.sub_link = SubAbilityLink::SequentialSibling;
                let branch = if shape == 0 {
                    amount
                } else {
                    let mut step = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 0 },
                            player: TargetFilter::ParentTarget,
                        },
                    );
                    if shape == 1 {
                        step.sub_ability = Some(Box::new(amount));
                    } else {
                        step.condition = Some(AbilityCondition::QuantityCheck {
                            lhs: QuantityExpr::Fixed { value: 0 },
                            comparator: Comparator::GT,
                            rhs: QuantityExpr::Fixed { value: 0 },
                        });
                        step.else_ability = Some(Box::new(amount));
                    }
                    step
                };
                let announced = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 0 },
                        player: TargetFilter::PlayerMatching {
                            player: Box::new(PlayerFilter::ControlsCount {
                                relation: PlayerRelation::All,
                                filter: TargetFilter::Typed(TypedFilter::creature()),
                                comparator: Comparator::GE,
                                count: Box::new(QuantityExpr::Fixed { value: 0 }),
                            }),
                        },
                    },
                )
                .sub_ability(synthetic_optional_search(branch));
                let announced = if prior {
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::PlayerMatching {
                                player: Box::new(PlayerFilter::All),
                            },
                        },
                    )
                    .sub_ability(announced)
                } else {
                    announced
                };
                let spell = scenario
                    .add_spell_to_hand(P0, "Paid inherited player", false)
                    .with_ability_definition(announced)
                    .with_mana_cost(ManaCost::generic(1))
                    .id();
                fund(&mut scenario, 1, &[]);
                let mut runner = scenario.build();
                runner.state_mut().players[0].poison_counters = 1;
                runner.state_mut().players[1].poison_counters = 2;
                runner
                    .cast(spell)
                    .target_players(if prior { &[P0, P1] } else { &[P1] })
                    .commit();
                let root = runner
                    .state()
                    .stack
                    .iter()
                    .find(|entry| entry.source_id == spell)
                    .unwrap()
                    .ability()
                    .unwrap();
                if prior {
                    assert_eq!(root.targets, vec![TargetRef::Player(P0)]);
                }
                let root = if prior {
                    root.sub_ability.as_deref().unwrap()
                } else {
                    root
                };
                assert_eq!(root.targets, vec![TargetRef::Player(P1)]);
                assert_eq!(root.target_reads, TargetReadOrigin::OwnAnnouncement);
                assert!(
                    matches!(&root.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots == &[0])
                );
                let search = root.sub_ability.as_deref().unwrap();
                let branch = search
                    .sub_ability
                    .as_deref()
                    .unwrap()
                    .sub_ability
                    .as_deref()
                    .unwrap()
                    .sub_ability
                    .as_deref()
                    .unwrap();
                assert!(branch.targets.is_empty());
                assert!(
                    matches!(&branch.chosen_target_slots, ChosenTargetSlots::Unrecorded)
                        || matches!(&branch.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots.is_empty())
                );
                let amount = if shape == 0 {
                    branch
                } else if shape == 1 {
                    branch.sub_ability.as_deref().unwrap()
                } else {
                    branch.else_ability.as_deref().unwrap()
                };
                assert!(amount.targets.is_empty() && amount.target_pins.is_empty());
                assert!(amount.effect_context_object.is_none());
                assert!(
                    matches!(&amount.chosen_target_slots, ChosenTargetSlots::Unrecorded)
                        || matches!(&amount.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots.is_empty())
                );
                assert_eq!(
                    runner.state().players[0]
                        .hand
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    hand.to_vec()
                );
                assert_eq!(
                    runner.state().players[1]
                        .hand
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    vec![opponent_hand]
                );
                assert!(runner.state().players[0].mana_pool.is_empty());
                for id in hand {
                    assert_eq!(runner.state().objects[&id].mana_cost.mana_value(), mv);
                }
                let mut events = Vec::new();
                pass_to_pause(&mut runner, &mut events);
                assert_optional_prompt(&runner);
                let parked = runner
                    .state()
                    .active_optional_effect_frame()
                    .unwrap()
                    .ability
                    .as_ref();
                assert_eq!(parked.targets, vec![TargetRef::Player(P1)]);
                let branch = parked
                    .sub_ability
                    .as_deref()
                    .unwrap()
                    .sub_ability
                    .as_deref()
                    .unwrap()
                    .sub_ability
                    .as_deref()
                    .unwrap();
                assert!(branch.targets.is_empty() && branch.target_pins.is_empty());
                let amount = if shape == 0 {
                    branch
                } else if shape == 1 {
                    branch.sub_ability.as_deref().unwrap()
                } else {
                    branch.else_ability.as_deref().unwrap()
                };
                assert!(amount.targets.is_empty() && amount.target_pins.is_empty());
                assert!(
                    matches!(&amount.chosen_target_slots, ChosenTargetSlots::Unrecorded)
                        || matches!(&amount.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots.is_empty())
                );
                assert!(amount.effect_context_object.is_none());
                assert_eq!(runner.state().players[0].poison_counters, 1);
                assert_eq!(runner.state().players[1].poison_counters, 2);
                assert!(runner.state().last_revealed_ids.is_empty());
                assert!(runner.state().players[0].mana_pool.is_empty());
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap();
                assert_eq!(
                    runner.state().players[0].life,
                    if prior { 24 } else { 23 },
                    "shape={shape}, parameter={parameter}, prior={prior}"
                );
                assert_eq!(
                    runner.state().players[1].life,
                    22,
                    "shape={shape}, parameter={parameter}"
                );
                assert_eq!(
                    runner.state().players[0]
                        .hand
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    hand.to_vec()
                );
                assert_eq!(
                    runner.state().players[1]
                        .hand
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    vec![opponent_hand]
                );
                for id in [found, cover] {
                    assert_eq!(runner.state().objects[&id].zone, Zone::Library);
                }
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                assert_settled(&runner);
            }
        }
    }
}

// CR 601.2h + CR 202.3: actual mana paid can differ from the printed mana value.
#[test]
fn declined_search_paid_target_metrics_keep_the_actual_payment_subject() {
    for metric in [
        CastManaSpentMetric::Total,
        CastManaSpentMetric::DistinctColors,
        CastManaSpentMetric::OfColor {
            color: ManaColor::Green,
        },
        CastManaSpentMetric::FromSource {
            source_filter: TargetFilter::Typed(TypedFilter::land()),
        },
    ] {
        let expected = if matches!(metric, CastManaSpentMetric::OfColor { .. }) {
            1
        } else {
            2
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Creature-only reducer", 0, 1)
            .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::generic(1),
                spell_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
                dynamic_count: None,
                reach: CostReductionReach::SpillsToGeneric,
            }));
        let target = scenario
            .add_creature_to_hand(P0, "Paid colored creature", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Green],
            })
            .id();
        let island = scenario.add_basic_land(P0, ManaColor::Blue);
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let colorless = scenario
            .add_land_from_oracle(P0, "Colorless land", "{T}: Add {C}.")
            .id();
        let hand = [
            scenario
                .add_spell_to_hand(P0, "First metric hand", false)
                .with_mana_cost(ManaCost::generic(expected))
                .id(),
            scenario
                .add_spell_to_hand(P0, "Second metric hand", false)
                .with_mana_cost(ManaCost::generic(expected))
                .id(),
        ];
        let found = scenario
            .add_spell_to_library_top(P0, "Candidate", false)
            .as_creature()
            .id();
        let cover = scenario.add_spell_to_library_top(P0, "Cover", false).id();
        let mut tail = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        let counter = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        scope: CountScope::Controller,
                        card_types: vec![],
                        filter: Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                            FilterProp::Cmc {
                                comparator: Comparator::EQ,
                                value: QuantityExpr::Ref {
                                    qty: QuantityRef::ManaSpentToCast {
                                        scope: CastManaObjectScope::AbilityTarget,
                                        metric: metric.clone(),
                                    },
                                },
                            },
                        ]))),
                    },
                },
            },
        )
        .sub_ability(tail);
        let spell = scenario
            .add_spell_to_hand(P0, "Noncreature paid search", false)
            .with_ability_definition(synthetic_optional_search(counter))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let mut runner = scenario.build();
        runner.cast(target).commit();
        assert_eq!(runner.state().objects[&target].mana_cost.mana_value(), 3);
        assert_eq!(runner.state().objects[&target].mana_spent_to_cast_amount, 2);
        assert_eq!(
            runner.state().objects[&target]
                .colors_spent_to_cast
                .distinct_colors(),
            2
        );
        assert_eq!(
            runner.state().objects[&target].colors_spent_to_cast.green,
            1
        );
        assert_eq!(
            runner.state().objects[&target]
                .mana_spent_source_snapshots
                .len(),
            2
        );
        let paid_sources = runner.state().objects[&target]
            .mana_spent_source_snapshots
            .iter()
            .map(|snapshot| snapshot.source_id)
            .collect::<Vec<_>>();
        assert_eq!(paid_sources.len(), 2);
        assert!(paid_sources.contains(&island) && paid_sources.contains(&forest));
        assert!(runner.state().objects[&island].tapped && runner.state().objects[&forest].tapped);
        assert!(!runner.state().objects[&colorless].tapped);
        let mut events = Vec::new();
        pass_to_pause(&mut runner, &mut events);
        assert_settled(&runner);
        assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
        runner.cast(spell).target_object(target).commit();
        assert_eq!(runner.state().objects[&spell].mana_spent_to_cast_amount, 1);
        assert!(runner.state().objects[&spell]
            .colors_spent_to_cast
            .is_empty());
        assert_eq!(
            runner.state().objects[&spell]
                .mana_spent_source_snapshots
                .len(),
            1
        );
        assert_eq!(
            runner.state().objects[&spell].mana_spent_source_snapshots[0].source_id,
            colorless
        );
        assert!(runner.state().objects[&colorless].tapped);
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            hand.to_vec()
        );
        assert!(runner.state().players[0].mana_pool.is_empty());
        let root = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == spell)
            .unwrap()
            .ability()
            .unwrap();
        let counter = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::PutCounter { .. }))
            .unwrap();
        assert_eq!(counter.targets, vec![TargetRef::Object(target)]);
        assert_eq!(counter.target_reads, TargetReadOrigin::OwnAnnouncement);
        assert!(
            matches!(&counter.chosen_target_slots, ChosenTargetSlots::Recorded(slots) if slots == &[0])
        );
        assert_eq!(
            counter.target_pins,
            vec![Some(ObjectIncarnationRef::from_object(
                &runner.state().objects[&target]
            ))]
        );
        assert_eq!(
            engine::game::quantity::resolve_quantity_with_targets(
                runner.state(),
                &QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: CastManaObjectScope::AbilityTarget,
                        metric: metric.clone()
                    }
                },
                counter
            ),
            expected as i32
        );
        assert_eq!(
            engine::game::quantity::resolve_quantity_with_targets(
                runner.state(),
                &QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast {
                        scope: CastManaObjectScope::SelfObject,
                        metric: metric.clone()
                    }
                },
                counter
            ),
            if matches!(
                metric,
                CastManaSpentMetric::Total | CastManaSpentMetric::FromSource { .. }
            ) {
                1
            } else {
                0
            }
        );
        pass_to_pause(&mut runner, &mut events);
        assert_optional_prompt(&runner);
        assert_eq!(runner.state().objects[&target].mana_spent_to_cast_amount, 2);
        assert_eq!(runner.state().objects[&spell].mana_spent_to_cast_amount, 1);
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            hand.to_vec()
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(
            runner.state().objects[&target]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&2)
        );
        for id in [found, cover, spell] {
            assert!(!runner.state().objects[&id]
                .counters
                .contains_key(&CounterType::Plus1Plus1));
        }
        assert_eq!(runner.state().players[0].life, 23);
        assert_eq!(runner.state().players[1].life, 20);
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            hand.to_vec()
        );
        for id in [found, cover] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_settled(&runner);
    }
}
