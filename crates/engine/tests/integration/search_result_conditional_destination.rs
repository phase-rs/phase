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

pub(super) fn load_finite_printed_records(records: &[&str]) -> engine::database::CardDatabase {
    use engine::database::{mtgjson::AtomicCard, synthesis::build_oracle_face, CardDatabase};
    let mut entries = serde_json::Map::new();
    for record in records {
        let atomic: AtomicCard = serde_json::from_str(record).unwrap();
        let face = build_oracle_face(&atomic, atomic.identifiers.scryfall_oracle_id.clone());
        entries.insert(
            face.name.to_lowercase(),
            serde_json::to_value(face).unwrap(),
        );
    }
    let serialized = serde_json::to_string(&entries).unwrap();
    let from_string = CardDatabase::from_json_str(&serialized).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &serialized).unwrap();
    let from_file = CardDatabase::from_export(file.path()).unwrap();
    for record in records {
        let atomic: AtomicCard = serde_json::from_str(record).unwrap();
        for database in [&from_string, &from_file] {
            let face = database.get_face_by_name(&atomic.name).unwrap();
            assert_eq!(face.oracle_text.as_deref(), atomic.text.as_deref());
            assert_eq!(
                serde_json::to_value(face).unwrap(),
                entries[&atomic.name.to_lowercase()]
            );
        }
    }
    from_string
}

#[test]
fn nashi_milled_choice_accepts_either_legendary_or_enchantment() {
    use engine::game::printed_cards::apply_card_face_to_object;
    use engine::types::card_type::{CoreType, Supertype};
    let database = load_finite_printed_records(FINITE_COHORT_PRINTED);
    let face = database
        .get_face_by_name("Nashi, Searcher in the Dark")
        .unwrap();
    assert!(engine::game::coverage::card_face_gaps(face).is_empty());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nashi = scenario
        .add_creature(P0, &face.name, 2, 3)
        .with_plus_counters(1)
        .id();
    scenario.with_library_top(
        P0,
        &[
            "Legendary nonenchantment",
            "Nonlegendary enchantment",
            "Neither",
        ],
    );
    let mut runner = scenario.build();
    apply_card_face_to_object(runner.state_mut().objects.get_mut(&nashi).unwrap(), face);
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), nashi);
    let library: Vec<_> = runner.state().players[0].library.iter().copied().collect();
    for (index, id) in library.iter().enumerate() {
        let object = runner.state_mut().objects.get_mut(id).unwrap();
        object.card_types.core_types = vec![if index == 1 {
            CoreType::Enchantment
        } else {
            CoreType::Sorcery
        }];
        object.card_types.supertypes = if index == 0 {
            vec![Supertype::Legendary]
        } else {
            vec![]
        };
        object.base_card_types = object.card_types.clone();
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(nashi, super::rules::AttackTarget::Player(P1))])
        .unwrap();
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).unwrap();
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::EffectZoneChoice { .. } => break,
            other => {
                panic!("Nashi's real combat trigger must reach its milled-card choice: {other:?}")
            }
        }
    }
    let WaitingFor::EffectZoneChoice { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Nashi did not offer milled cards: {:?}",
            runner.state().waiting_for
        )
    };
    assert!(
        cards.contains(&library[0]),
        "legendary is an independent union leg"
    );
    assert!(
        cards.contains(&library[1]),
        "enchantment is an independent union leg"
    );
    assert!(!cards.contains(&library[2]));
    runner
        .act(GameAction::SelectCards {
            cards: vec![library[0], library[1]],
        })
        .unwrap();
    pass_to_pause(&mut runner, &mut vec![]);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&library[0]].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&library[1]].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&library[2]].zone, Zone::Graveyard);
    assert_eq!(
        runner.counters(nashi, CounterType::Plus1Plus1),
        1,
        "taking cards skips the no-cards counter branch"
    );
}

#[test]
fn finite_qualified_cohorts_survive_full_string_and_file_loaders() {
    use engine::types::ability::DigRestOrder;
    let database = load_finite_printed_records(FINITE_COHORT_PRINTED);
    for name in [
        "Kayla's Reconstruction",
        "Genesis Wave",
        "Saheeli's Directive",
        "Collected Company",
        "Pieces of the Puzzle",
    ] {
        let face = database.get_face_by_name(name).unwrap();
        assert!(
            engine::game::coverage::card_face_gaps(face).is_empty(),
            "{name}: {:?}",
            engine::game::coverage::card_face_gaps(face)
        );
        let dig = &face.abilities[0];
        let Effect::Dig {
            keep_count,
            keep_count_expr,
            up_to,
            destination,
            rest_destination,
            rest_order,
            ..
        } = &*dig.effect
        else {
            panic!("{name}: {dig:#?}")
        };
        assert!(*up_to);
        if name == "Kayla's Reconstruction" {
            assert_eq!(*keep_count, None);
            assert_eq!(
                *keep_count_expr,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name: "X".into() }
                })
            );
            assert_eq!(*rest_order, DigRestOrder::Random);
        }
        if matches!(
            name,
            "Genesis Wave" | "Saheeli's Directive" | "Pieces of the Puzzle"
        ) {
            assert_eq!(*rest_destination, Some(Zone::Graveyard));
            assert!(
                dig.sub_ability.is_none(),
                "the rejected reveal remainder belongs to its Dig: {dig:#?}"
            );
        } else {
            assert_eq!(*rest_destination, Some(Zone::Library));
        }
        if name == "Collected Company" {
            assert_eq!(*rest_order, DigRestOrder::PlayerChoice);
            assert_eq!(*keep_count, Some(2));
        }
        assert_eq!(
            *destination,
            Some(if name == "Pieces of the Puzzle" {
                Zone::Hand
            } else {
                Zone::Battlefield
            })
        );
    }
    let hunting = database.get_face_by_name("Hunting Wilds").unwrap();
    assert!(!engine::game::coverage::card_face_gaps(hunting).is_empty());
    let mut search = false;
    let mut shuffle = false;
    let mut dependent_gap = false;
    for ability in &hunting.abilities {
        let _ = engine::types::ability_visit::visit_ability_def(ability, &mut |effect| {
            search |= matches!(effect, Effect::SearchLibrary { .. });
            shuffle |= matches!(effect, Effect::Shuffle { .. });
            dependent_gap |= matches!(effect, Effect::Unimplemented { name, .. } if name == "search_destination_cohort");
            assert!(
                !matches!(
                    effect,
                    Effect::SetTapState {
                        state: engine::types::ability::TapStateChange::Untap,
                        ..
                    } | Effect::GenericEffect { .. }
                ),
                "an unbound searched cohort cannot untap or animate ambient Forests"
            );
            std::ops::ControlFlow::<()>::Continue(())
        });
    }
    assert!(
        search && shuffle && dependent_gap,
        "independent search survives the atomic dependent gap"
    );
}

#[test]
fn finite_dig_choices_use_announced_x_rejected_remainder_and_owner_order() {
    use engine::game::printed_cards::apply_card_face_to_object;
    use engine::types::card_type::CoreType;
    let database = load_finite_printed_records(FINITE_COHORT_PRINTED);
    for name in [
        "Kayla's Reconstruction",
        "Genesis Wave",
        "Collected Company",
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        fund(
            &mut scenario,
            4,
            &[
                ManaType::White,
                ManaType::White,
                ManaType::White,
                ManaType::Green,
                ManaType::Green,
                ManaType::Green,
            ],
        );
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, name, name == "Collected Company", "")
            .id();
        scenario.with_library_top(
            P0,
            &[
                "Allowed one",
                "Allowed two",
                "Rejected high",
                "Rejected spell",
                "Rest one",
                "Rest two",
                "Rest three",
                "Untouched",
            ],
        );
        let mut runner = scenario.build();
        apply_card_face_to_object(
            runner.state_mut().objects.get_mut(&spell).unwrap(),
            database.get_face_by_name(name).unwrap(),
        );
        let library: Vec<_> = runner.state().players[0].library.iter().copied().collect();
        for (index, id) in library.iter().enumerate() {
            let object = runner.state_mut().objects.get_mut(id).unwrap();
            object.card_types.core_types = vec![if index == 3 {
                CoreType::Sorcery
            } else {
                CoreType::Creature
            }];
            object.base_card_types = object.card_types.clone();
            object.mana_cost = ManaCost::generic(if index == 2 { 8 } else { 1 });
            object.base_mana_cost = object.mana_cost.clone();
            object.power = Some(1);
            object.base_power = Some(1);
            object.toughness = Some(1);
            object.base_toughness = Some(1);
        }
        if name == "Collected Company" {
            runner.cast(spell).commit();
        } else {
            runner
                .cast(spell)
                .x(if name == "Genesis Wave" { 4 } else { 2 })
                .commit();
        }
        pass_to_pause(&mut runner, &mut vec![]);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
        }
        let WaitingFor::DigChoice {
            keep_count,
            selectable_cards,
            rest_destination,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real Dig choice: {:?}", runner.state().waiting_for)
        };
        if name == "Genesis Wave" {
            assert!(*keep_count >= 2);
        } else {
            assert_eq!(*keep_count, 2);
        }
        assert!(selectable_cards.contains(&library[0]) && selectable_cards.contains(&library[1]));
        assert!(!selectable_cards.contains(&library[2]));
        assert_eq!(
            *rest_destination,
            Some(if name == "Genesis Wave" {
                Zone::Graveyard
            } else {
                Zone::Library
            })
        );
        runner
            .act(GameAction::SelectCards {
                cards: vec![library[0], library[1]],
            })
            .unwrap();
        if name == "Collected Company" {
            let WaitingFor::DigRestSplitChoice {
                cards, top_count, ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "the owner must choose the four-card rest order: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(*top_count, 0);
            let mut order = cards.clone();
            order.reverse();
            runner
                .act(GameAction::SelectCards {
                    cards: order.clone(),
                })
                .unwrap();
            let actual: Vec<_> = runner.state().players[0].library.iter().copied().collect();
            assert_eq!(&actual[actual.len() - order.len()..], order.as_slice());
        }
        pass_to_pause(&mut runner, &mut vec![]);
        assert_settled(&runner);
        assert_eq!(runner.state().objects[&library[0]].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&library[1]].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&library[2]].zone,
            if name == "Genesis Wave" {
                Zone::Graveyard
            } else {
                Zone::Library
            }
        );
        if name == "Genesis Wave" {
            assert_eq!(runner.state().objects[&library[3]].zone, Zone::Graveyard);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    }
}

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
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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

#[test]
fn conjunctive_search_pays_for_five_distinct_matching_slots_and_rejects_duplicates() {
    // CR 608.2c + CR 701.23a: each printed card description owns one search member.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Conflux", false,
        "Search your library for a white card, a blue card, a black card, a red card, and a green card. Reveal those cards, put them into your hand, then shuffle.")
        .with_mana_cost(super::jev_defining_destroy::cost("{3}{W}{U}{B}{R}{G}")).id();
    fund(
        &mut scenario,
        3,
        &[
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ],
    );
    let mut choices = Vec::new();
    for (name, color) in [
        ("White one", ManaColor::White),
        ("Blue", ManaColor::Blue),
        ("Black", ManaColor::Black),
        ("Red", ManaColor::Red),
        ("Green", ManaColor::Green),
        ("White two", ManaColor::White),
    ] {
        choices.push(
            scenario
                .add_spell_to_library_top(P0, name, true)
                .with_color(vec![color])
                .id(),
        );
    }
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    let mut events = Vec::new();
    pass_to_pause(&mut runner, &mut events);
    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        reveal,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Conflux must reach one complete search: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 5);
    assert!(*reveal);
    assert_eq!(cards.len(), 6);
    for id in &choices {
        assert!(cards.contains(id));
    }
    let before = serde_json::to_value(runner.state()).unwrap();
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![choices[0], choices[5], choices[2], choices[3], choices[4]]
            })
            .is_err(),
        "two white-only cards cannot fill the missing blue slot"
    );
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    let selected = vec![choices[4], choices[2], choices[0], choices[3], choices[1]];
    let last = runner
        .act(GameAction::SelectCards {
            cards: selected.clone(),
        })
        .unwrap()
        .events;
    assert_complete(&runner, &last);
    for id in &selected {
        assert_eq!(runner.state().objects[id].zone, Zone::Hand);
    }
    assert_eq!(runner.state().objects[&choices[5]].zone, Zone::Library);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    let revealed = last
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids),
            _ => None,
        })
        .flatten()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(revealed, selected.into_iter().collect());
}

const CONJUNCTIVE_SEARCH_PRINTED: &[(i32, &str)] = &[
    (
        2,
        r###"{"name":"Yasharn, Implacable Earth","manaCost":"{2}{G}{W}","manaValue":4.0,"type":"Legendary Creature — Elemental Boar","types":["Creature"],"subtypes":["Elemental","Boar"],"supertypes":["Legendary"],"colors":["G","W"],"colorIdentity":["G","W"],"power":"4","toughness":"4","layout":"normal","text":"When Yasharn enters, search your library for a basic Forest card and a basic Plains card, reveal those cards, put them into your hand, then shuffle.\nPlayers can't pay life or sacrifice nonland permanents to cast spells or activate abilities.","identifiers":{"scryfallOracleId":"6dc23410-4d8f-4c8a-92b4-a8b3fae1af4e"}}"###,
    ),
    (
        5,
        r###"{"name":"Gaea's Balance","manaCost":"{3}{G}","manaValue":4.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"As an additional cost to cast this spell, sacrifice five lands.\nSearch your library for a land card of each basic land type, put those cards onto the battlefield, then shuffle.","identifiers":{"scryfallOracleId":"5aadbd03-c459-4893-aa1b-efcf193faa1b"}}"###,
    ),
    (
        2,
        r###"{"name":"Corpse Harvester","manaCost":"{3}{B}{B}","manaValue":5.0,"type":"Creature — Zombie Wizard","types":["Creature"],"subtypes":["Zombie","Wizard"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"3","layout":"normal","text":"{1}{B}, {T}, Sacrifice a creature: Search your library for a Zombie card and a Swamp card, reveal them, put them into your hand, then shuffle.","identifiers":{"scryfallOracleId":"f02ad6fa-7a82-471a-979e-0bb9ba866ef6"}}"###,
    ),
    (
        2,
        r###"{"name":"Krosan Verge","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"This land enters tapped.\n{T}: Add {C}.\n{2}, {T}, Sacrifice this land: Search your library for a Forest card and a Plains card, put them onto the battlefield tapped, then shuffle.","identifiers":{"scryfallOracleId":"d9a10971-f32b-4978-952d-fed0a5bc9e36"}}"###,
    ),
    (
        5,
        r###"{"name":"Conflux","manaCost":"{3}{W}{U}{B}{R}{G}","manaValue":8.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["B","G","R","U","W"],"colorIdentity":["B","G","R","U","W"],"layout":"normal","text":"Search your library for a white card, a blue card, a black card, a red card, and a green card. Reveal those cards, put them into your hand, then shuffle.","identifiers":{"scryfallOracleId":"32ebb029-9f03-49c9-b9bb-cb1954e2a324"}}"###,
    ),
    (
        3,
        r###"{"name":"Lotuslight Dancers","manaCost":"{2}{B}{G}{U}","manaValue":5.0,"type":"Creature — Zombie Bard","types":["Creature"],"subtypes":["Zombie","Bard"],"supertypes":[],"colors":["B","G","U"],"colorIdentity":["B","G","U"],"power":"3","toughness":"6","layout":"normal","text":"Lifelink\nWhen this creature enters, search your library for a black card, a green card, and a blue card. Put those cards into your graveyard, then shuffle.","keywords":["Lifelink"],"identifiers":{"scryfallOracleId":"83c8f1ca-194e-4cb6-9d5f-029133c1edbc"}}"###,
    ),
    (
        3,
        r###"{"name":"Gem of Becoming","manaCost":"{3}","manaValue":3.0,"type":"Artifact","types":["Artifact"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{3}, {T}, Sacrifice this artifact: Search your library for an Island card, a Swamp card, and a Mountain card. Reveal those cards, put them into your hand, then shuffle.","identifiers":{"scryfallOracleId":"6b1e4940-11ac-4d85-98f4-3f50d1060b96"}}"###,
    ),
    (
        4,
        r###"{"name":"Shard Convergence","manaCost":"{3}{G}","manaValue":4.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Search your library for a Plains card, an Island card, a Swamp card, and a Mountain card. Reveal those cards, put them into your hand, then shuffle.","identifiers":{"scryfallOracleId":"c51fc4f2-76a6-4337-ab8f-1b3e84f82019"}}"###,
    ),
    (
        3,
        r###"{"name":"Nissa's Encouragement","manaCost":"{4}{G}","manaValue":5.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Search your library and graveyard for a card named Forest, a card named Brambleweft Behemoth, and a card named Nissa, Genesis Mage. Reveal those cards, put them into your hand, then shuffle.","identifiers":{"scryfallOracleId":"55e897e9-5c70-4e32-8f24-3e0fefa319cd"}}"###,
    ),
    (
        2,
        r###"{"name":"Aang's Journey","manaCost":"{2}","manaValue":2.0,"type":"Sorcery — Lesson","types":["Sorcery"],"subtypes":["Lesson"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"Kicker {2} (You may pay an additional {2} as you cast this spell.)\nSearch your library for a basic land card. If this spell was kicked, instead search your library for a basic land card and a Shrine card. Reveal those cards, put them into your hand, then shuffle.\nYou gain 2 life.","keywords":["Kicker"],"identifiers":{"scryfallOracleId":"ddf461b9-a205-4dc7-a9c3-c047b1ff709f"}}"###,
    ),
];

#[test]
fn complete_search_faces_retain_count_and_independent_filter_slots() {
    // SHAPE: complete-card filter/count preservation supplements the paid Conflux regression.
    use engine::database::{mtgjson::AtomicCard, synthesis::build_oracle_face};
    for (expected, printed) in CONJUNCTIVE_SEARCH_PRINTED {
        let card: AtomicCard = serde_json::from_str(printed).unwrap();
        let face = build_oracle_face(&card, card.identifiers.scryfall_oracle_id.clone());
        let mut found = false;
        for ability in face.abilities.iter().chain(
            face.triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref()),
        ) {
            let _ = engine::types::ability_visit::visit_ability_def(ability, &mut |effect| {
                if let Effect::SearchLibrary {
                    count: QuantityExpr::Fixed { value },
                    selection_constraint: SearchSelectionConstraint::MatchEachFilter { filters },
                    ..
                } = effect
                {
                    if value == expected {
                        assert_eq!(filters.len(), *expected as usize, "{}", face.name);
                        let leaf = |index: usize| {
                            let TargetFilter::Typed(typed) = &filters[index] else {
                                panic!("independent described search member: {:?}", filters[index]);
                            };
                            typed
                        };
                        let basic = FilterProp::HasSupertype {
                            value: engine::types::card_type::Supertype::Basic,
                        };
                        match face.name.as_str() {
                            "Yasharn, Implacable Earth" => {
                                assert!(leaf(0)
                                    .type_filters
                                    .contains(&TypeFilter::Subtype("Forest".into())));
                                assert!(leaf(1)
                                    .type_filters
                                    .contains(&TypeFilter::Subtype("Plains".into())));
                                assert!(
                                    leaf(0).properties.contains(&basic)
                                        && leaf(1).properties.contains(&basic)
                                );
                            }
                            "Gaea's Balance" => {
                                for (index, name) in
                                    ["Plains", "Island", "Swamp", "Mountain", "Forest"]
                                        .iter()
                                        .enumerate()
                                {
                                    assert!(leaf(index)
                                        .type_filters
                                        .contains(&TypeFilter::Subtype((*name).into())));
                                    assert!(
                                        !leaf(index).properties.contains(&basic),
                                        "a basic land type does not require a basic land"
                                    );
                                }
                            }
                            "Aang's Journey" => {
                                assert!(leaf(0).properties.contains(&basic));
                                assert!(leaf(1)
                                    .type_filters
                                    .contains(&TypeFilter::Subtype("Shrine".into())));
                                assert!(
                                    !leaf(1).properties.contains(&basic),
                                    "basic is local to the land member"
                                );
                            }
                            "Nissa's Encouragement" => {
                                for (index, name) in
                                    ["forest", "brambleweft behemoth", "nissa, genesis mage"]
                                        .iter()
                                        .enumerate()
                                {
                                    assert!(leaf(index).properties.iter().any(|property| matches!(property, FilterProp::Named { name: actual } if actual.eq_ignore_ascii_case(name))));
                                }
                            }
                            "Gem of Becoming" | "Shard Convergence" => {
                                let expected = if face.name == "Gem of Becoming" {
                                    vec!["Island", "Swamp", "Mountain"]
                                } else {
                                    vec!["Plains", "Island", "Swamp", "Mountain"]
                                };
                                for (index, name) in expected.iter().enumerate() {
                                    assert!(leaf(index)
                                        .type_filters
                                        .contains(&TypeFilter::Subtype((*name).into())));
                                }
                            }
                            _ => {}
                        }
                        found = true;
                    }
                }
                std::ops::ControlFlow::Continue(())
            });
        }
        assert!(
            found,
            "whole Oracle search must reach all printed slots: {}",
            face.name
        );
    }
}

// Complete unchanged records from pinned MTGJSON 2026-10-04, version 5.3.0+20261004.
const FINITE_COHORT_PRINTED: &[&str] = &[
    r####"{"colorIdentity":["W"],"colors":["W"],"convertedManaCost":3.0,"edhrecRank":18392,"edhrecSaltiness":0.0,"foreignData":[{"language":"Chinese Simplified","name":"\u51ef\u62c9\u7684\u91cd\u7b51","skuIds":{"foil":"a0cae147-da45-593e-bace-24adcddc7937","nonfoil":"c41bdb71-1f10-5cd5-89ca-b0636e6e4512"},"text":"\u68c0\u89c6\u4f60\u724c\u5e93\u9876\u7684\u4e03\u5f20\u724c\u3002\u5c06\u5176\u4e2d\u81f3\u591aX\u5f20\u6cd5\u672f\u529b\u503c\u7b49\u4e8e\u6216\u5c0f\u4e8e3\u7684\u795e\u5668\u548c\uff0f\u6216\u751f\u7269\u724c\u653e\u8fdb\u6218\u573a\u3002\u5c06\u5176\u4f59\u7684\u724c\u4ee5\u968f\u673a\u987a\u5e8f\u7f6e\u4e8e\u4f60\u724c\u5e93\u5e95\u3002","type":"\u6cd5\u672f"},{"language":"French","name":"Reconstruction de Kayla","skuIds":{"foil":"d45b9013-5d21-5809-8fc7-2beb3735eb87","nonfoil":"7615ec72-7972-5409-a436-35caf0e73325"},"text":"Regardez les sept cartes du dessus de votre biblioth\u00e8que. Mettez sur le champ de bataille jusqu'\u00e0 X cartes d'artefact et/ou de cr\u00e9ature avec une valeur de mana inf\u00e9rieure ou \u00e9gale \u00e0 3 parmi elles. Mettez le reste au-dessous de votre biblioth\u00e8que dans un ordre al\u00e9atoire.","type":"Rituel"},{"language":"German","name":"Kaylas Wiederaufbau","skuIds":{"foil":"3bad8c9c-7cef-5901-a884-626e64673c10","nonfoil":"992d6cfa-01a3-58bb-a5ce-7c51e31aace9"},"text":"Schaue dir die obersten sieben Karten deiner Bibliothek an. Bringe davon bis zu X Artefakt- und/oder Kreaturenkarten mit Manabetrag 3 oder weniger ins Spiel. Lege den Rest in zuf\u00e4lliger Reihenfolge unter deine Bibliothek.","type":"Hexerei"},{"language":"Italian","name":"Ricostruzione di Kayla","skuIds":{"foil":"43517f9f-fedd-5cd2-8cd1-994632729823","nonfoil":"291fc4c9-9c0f-5bb5-ab85-667e532c1f83"},"text":"Guarda le prime sette carte del tuo grimorio. Metti sul campo di battaglia fino a X carte creatura e/o artefatto con valore di mana pari o inferiore a 3 scelte tra esse. Metti le altre in fondo al tuo grimorio in ordine casuale.","type":"Stregoneria"},{"language":"Japanese","name":"\u30ab\u30a4\u30e9\u306e\u518d\u5efa","skuIds":{"foil":"6473b500-5e97-50b5-b059-4617ede8626b","nonfoil":"688b4801-a1e1-523d-8baf-9f276f0f13e3"},"text":"\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0a\u306b\u3042\u308b\u30ab\u30fc\u30c9\uff17\u679a\u3092\u898b\u308b\u3002\u305d\u306e\u4e2d\u304b\u3089\u30de\u30ca\u7dcf\u91cf\u304c\uff13\u4ee5\u4e0b\u3067\u3042\u308a\u30a2\u30fc\u30c6\u30a3\u30d5\u30a1\u30af\u30c8\u3084\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u3067\u3042\u308b\u30ab\u30fc\u30c9\u6700\u5927\uff38\u679a\u3092\u6226\u5834\u306b\u51fa\u3059\u3002\u6b8b\u308a\u3092\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0b\u306b\u7121\u4f5c\u70ba\u306e\u9806\u756a\u3067\u7f6e\u304f\u3002","type":"\u30bd\u30fc\u30b5\u30ea\u30fc"},{"language":"Portuguese (Brazil)","name":"Reconstru\u00e7\u00e3o de Kayla","skuIds":{"foil":"cfa871d9-69e7-512b-9cea-83f079ef844a","nonfoil":"b8d20f93-d78e-5d5c-9d66-60da838e45d2"},"text":"Olhe os sete cards do topo de seu grim\u00f3rio. Coloque no campo de batalha at\u00e9 X cards de criatura e/ou artefato com valor de mana igual ou inferior a 3 dentre eles. Coloque o restante no fundo de seu grim\u00f3rio em ordem aleat\u00f3ria.","type":"Feiti\u00e7o"},{"language":"Spanish","name":"Reconstrucci\u00f3n de Kayla","skuIds":{"foil":"31648920-6d6b-5374-b034-1d45bbd14aec","nonfoil":"f3a7671b-5c76-5f13-8d0d-8059788f0928"},"text":"Mira las siete primeras cartas de tu biblioteca. Pon en el campo de batalla hasta X cartas de artefacto y/o de criatura con valor de man\u00e1 de 3 o menos de entre ellas. Pon el resto en el fondo de tu biblioteca en un orden aleatorio.","type":"Conjuro"}],"identifiers":{"scryfallOracleId":"43bae4b3-57e8-4d9a-9d1b-9805b0e0fff5"},"layout":"normal","legalities":{"brawl":"Legal","commander":"Legal","duel":"Legal","gladiator":"Legal","historic":"Legal","legacy":"Legal","modern":"Legal","oathbreaker":"Legal","penny":"Legal","pioneer":"Legal","timeless":"Legal","vintage":"Legal"},"manaCost":"{X}{W}{W}{W}","manaValue":3.0,"name":"Kayla's Reconstruction","printings":["BRO","PBRO","PRM"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/e9be6f5e1f00fba0","cardKingdomFoil":"https://mtgjson.com/links/e683c48c27fa9eb1","cardmarket":"https://mtgjson.com/links/aa9cf5f44081b69d","cardmarketFoil":"https://mtgjson.com/links/485e76e0d05590e1","tcgplayer":"https://mtgjson.com/links/f902edbdbd0ea50a"},"rulings":[{"date":"2022-10-14","text":"Each of the cards needs to have mana value 3 or less. It's not the total mana value of the cards."}],"subtypes":[],"supertypes":[],"text":"Look at the top seven cards of your library. Put up to X artifact and/or creature cards with mana value 3 or less from among them onto the battlefield. Put the rest on the bottom of your library in a random order.","type":"Sorcery","types":["Sorcery"]}"####,
    r####"{"colorIdentity":["G"],"colors":["G"],"convertedManaCost":3.0,"edhrecRank":1364,"edhrecSaltiness":0.49,"foreignData":[{"language":"Chinese Simplified","name":"\u521b\u751f\u6d6a\u6f6e","skuIds":{"foil":"9ac32b10-5d1d-5a42-88b4-57bc34970dfb","nonfoil":"213d3a38-0c0b-59b7-855e-54323660ebdd"},"text":"\u5c55\u793a\u4f60\u724c\u5e93\u9876\u7684X\u5f20\u724c\u3002 \u4f60\u53ef\u4ee5\u5c06\u5176\u4e2d\u4efb\u610f\u6570\u91cf\u603b\u6cd5\u672f\u529b\u8d39\u7528\u5c0f\u4e8e\u6216\u7b49\u4e8eX\u7684\u6c38\u4e45\u7269\u724c\u653e\u8fdb\u6218\u573a\u3002 \u7136\u540e\u5c06\u6240\u6709\u4ee5\u6b64\u6cd5\u5c55\u793a\u3001\u4e14\u672a\u653e\u8fdb\u6218\u573a\u7684\u724c\u7f6e\u5165\u4f60\u7684\u575f\u5893\u573a\u3002","type":"\u6cd5\u672f"},{"language":"Chinese Traditional","name":"\u5275\u751f\u6d6a\u6f6e","skuIds":{"foil":"f636e410-be75-55d9-a3da-b7d465e2e266","nonfoil":"aed32f24-6674-5f1c-8a14-91057a638e13"},"text":"\u5c55\u793a\u4f60\u724c\u5eab\u9802\u7684X\u5f35\u724c\u3002 \u4f60\u53ef\u4ee5\u5c07\u5176\u4e2d\u4efb\u610f\u6578\u91cf\u7e3d\u9b54\u6cd5\u529b\u8cbb\u7528\u5c0f\u65bc\u6216\u7b49\u65bcX\u7684\u6c38\u4e45\u7269\u724c\u653e\u9032\u6230\u5834\u3002 \u7136\u5f8c\u5c07\u6240\u6709\u4ee5\u6b64\u6cd5\u5c55\u793a\u3001\u4e14\u672a\u653e\u9032\u6230\u5834\u7684\u724c\u7f6e\u5165\u4f60\u7684\u58b3\u5893\u5834\u3002","type":"\u5deb\u8853"},{"language":"French","name":"Vague de gen\u00e8se","skuIds":{"foil":"bdff8f20-038a-57a5-9daa-9aad212575db","nonfoil":"80230d25-5399-5c54-bbcb-7274f73b748c"},"text":"R\u00e9v\u00e9lez les X cartes du dessus de votre biblioth\u00e8que. Parmi elles, vous pouvez mettre sur le champ de bataille n'importe quel nombre de cartes de permanent de co\u00fbt converti de mana inf\u00e9rieur ou \u00e9gal \u00e0 X. Mettez ensuite toutes les cartes r\u00e9v\u00e9l\u00e9es de cette mani\u00e8re qui n'ont pas \u00e9t\u00e9 mises sur le champ de bataille dans votre cimeti\u00e8re.","type":"Rituel"},{"language":"German","name":"Genesiswelle","skuIds":{"foil":"32cdb55d-8c3f-58f2-9e03-32c7dbb992bd","nonfoil":"77300603-7ae2-50e6-80ce-94dc98e25e43"},"text":"Decke die obersten X Karten deiner Bibliothek auf. Du kannst davon eine beliebige Anzahl an bleibenden Karten mit umgewandelten Manakosten von X oder weniger ins Spiel bringen. Lege dann alle Karten, die auf diese Weise aufgedeckt wurden und nicht ins Spiel gebrcht wurden, auf deinen Friedhof.","type":"Hexerei"},{"language":"Italian","name":"Flusso d'Origine","skuIds":{"foil":"f031d4f5-9721-5b23-8fd3-c0e2d0da6e7e","nonfoil":"936d7ddc-5f0c-5cb3-933f-821fceaadb55"},"text":"Rivela le prime X carte del tuo grimorio. Puoi mettere sul campo di battaglia un qualsiasi numero di carte permanente con costo di mana convertito pari o inferiore a X scelte tra quelle carte rivelate. Poi metti nel tuo cimitero tutte le carte rivelate in questo modo che non sono state messe sul campo di battaglia.","type":"Stregoneria"},{"language":"Japanese","name":"\u8d77\u6e90\u306e\u6ce2","skuIds":{"foil":"76d93379-4757-5de0-bb0f-5577168bf622","nonfoil":"1525b17b-c80c-5162-bd4e-c09a3f137339"},"text":"\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0a\u304b\u3089\uff38\u679a\u306e\u30ab\u30fc\u30c9\u3092\u516c\u958b\u3059\u308b\u3002 \u3042\u306a\u305f\u306f\u305d\u306e\u4e2d\u306e\u70b9\u6570\u3067\u898b\u305f\u30de\u30ca\u30fb\u30b3\u30b9\u30c8\u304c\uff38\u4ee5\u4e0b\u3067\u3042\u308b\u30d1\u30fc\u30de\u30cd\u30f3\u30c8\u30fb\u30ab\u30fc\u30c9\u3092\u3001\u671b\u3080\u679a\u6570\u6226\u5834\u306b\u51fa\u3057\u3066\u3082\u3088\u3044\u3002 \u305d\u306e\u5f8c\u3001\u3053\u308c\u306b\u3088\u308a\u516c\u958b\u3055\u308c\u3066\u6226\u5834\u306b\u51fa\u3055\u308c\u306a\u304b\u3063\u305f\u3059\u3079\u3066\u306e\u30ab\u30fc\u30c9\u3092\u3042\u306a\u305f\u306e\u5893\u5730\u306b\u7f6e\u304f\u3002","type":"\u30bd\u30fc\u30b5\u30ea\u30fc"},{"language":"Portuguese (Brazil)","name":"Onda de G\u00eanese","skuIds":{"foil":"6c1458d4-6c5c-571e-a1fb-620877c28c69","nonfoil":"9376778f-6afb-5049-ab18-17694477ac7e"},"text":"Revele os primeiros X cards do topo de seu grim\u00f3rio. Voc\u00ea pode colocar qualquer n\u00famero de cards de permanente com custo de mana convertido menor ou igual a X dentre eles no campo de batalha. Depois, coloque no seu cemit\u00e9rio todos os cards revelados dessa maneira que n\u00e3o foram colocados no campo de batalha.","type":"Feiti\u00e7o"},{"language":"Russian","name":"\u0412\u043e\u043b\u043d\u0430 \u0417\u0430\u0440\u043e\u0436\u0434\u0435\u043d\u0438\u044f","skuIds":{"foil":"d56d6d6a-2244-5b56-9bbb-73870ee7a0f1","nonfoil":"96b0d5c1-6231-59bd-9bc4-03c842af1d3d"},"text":"\u041f\u043e\u043a\u0430\u0436\u0438\u0442\u0435 X \u0432\u0435\u0440\u0445\u043d\u0438\u0445 \u043a\u0430\u0440\u0442 \u0432\u0430\u0448\u0435\u0439 \u0431\u0438\u0431\u043b\u0438\u043e\u0442\u0435\u043a\u0438. \u0412\u044b \u043c\u043e\u0436\u0435\u0442\u0435 \u043f\u043e\u043b\u043e\u0436\u0438\u0442\u044c \u043d\u0430 \u043f\u043e\u043b\u0435 \u0431\u0438\u0442\u0432\u044b \u043b\u044e\u0431\u043e\u0435 \u043a\u043e\u043b\u0438\u0447\u0435\u0441\u0442\u0432\u043e \u043d\u0430\u0445\u043e\u0434\u044f\u0449\u0438\u0445\u0441\u044f \u0441\u0440\u0435\u0434\u0438 \u043d\u0438\u0445 \u043a\u0430\u0440\u0442 \u043f\u0435\u0440\u043c\u0430\u043d\u0435\u043d\u0442\u043e\u0432 \u0441 \u043a\u043e\u043d\u0432\u0435\u0440\u0442\u0438\u0440\u043e\u0432\u0430\u043d\u043d\u043e\u0439 \u043c\u0430\u043d\u0430-\u0441\u0442\u043e\u0438\u043c\u043e\u0441\u0442\u044c\u044e \u043d\u0435 \u0431\u043e\u043b\u0435\u0435 X. \u0417\u0430\u0442\u0435\u043c \u043f\u043e\u043b\u043e\u0436\u0438\u0442\u0435 \u0432\u0441\u0435 \u043f\u043e\u043a\u0430\u0437\u0430\u043d\u043d\u044b\u0435 \u0442\u0430\u043a\u0438\u043c \u043e\u0431\u0440\u0430\u0437\u043e\u043c \u043a\u0430\u0440\u0442\u044b, \u043a\u043e\u0442\u043e\u0440\u044b\u0435 \u043d\u0435 \u0431\u044b\u043b\u0438 \u043f\u043e\u043b\u043e\u0436\u0435\u043d\u044b \u043d\u0430 \u043f\u043e\u043b\u0435 \u0431\u0438\u0442\u0432\u044b, \u043d\u0430 \u0432\u0430\u0448\u0435 \u043a\u043b\u0430\u0434\u0431\u0438\u0449\u0435.","type":"\u0412\u043e\u043b\u0448\u0435\u0431\u0441\u0442\u0432\u043e"},{"language":"Spanish","name":"Ola de g\u00e9nesis","skuIds":{"foil":"492d03e8-b33e-550d-b93c-6704265f17b9","nonfoil":"417ba390-0b4e-5358-b32d-e63cbdd965d7"},"text":"Muestra las primeras X cartas de tu biblioteca. Puedes poner en el campo de batalla cualquier cantidad de cartas de permanente con coste de man\u00e1 convertido de X o menos que se encuentren entre ellas. Luego pon en tu cementerio todas las cartas mostradas de esta manera que no fueron puestas en el campo de batalla.","type":"Conjuro"}],"identifiers":{"scryfallOracleId":"e2487868-f386-438e-a73f-b494f6d35fac"},"layout":"normal","legalities":{"alchemy":"Legal","brawl":"Legal","commander":"Legal","duel":"Legal","future":"Legal","gladiator":"Legal","historic":"Legal","legacy":"Legal","modern":"Legal","oathbreaker":"Legal","penny":"Legal","pioneer":"Legal","predh":"Legal","standard":"Legal","standardbrawl":"Legal","timeless":"Legal","vintage":"Legal"},"manaCost":"{X}{G}{G}{G}","manaValue":3.0,"name":"Genesis Wave","printings":["FDN","IMA","LTC","PFDN","SOM"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/71dbee38a67e4011","cardKingdomFoil":"https://mtgjson.com/links/8d6fa02d464cfb9b","cardmarket":"https://mtgjson.com/links/3dd97061c7753fdf","cardmarketFoil":"https://mtgjson.com/links/1b14b0eaa669d6da","tcgplayer":"https://mtgjson.com/links/6cf6fc2859f6ead5"},"rulings":[{"date":"2024-11-08","text":"A permanent card is an artifact, battle, creature, enchantment, land, or planeswalker card."},{"date":"2024-11-08","text":"All of the permanents put onto the battlefield this way enter at the same time. If any have triggered abilities that trigger on something else entering, they'll see each other."},{"date":"2024-11-08","text":"If a card in a player's library has {X} in its mana cost, X is considered to be 0."},{"date":"2024-11-08","text":"If a permanent card in your library has no mana symbols in its upper right corner (because it's a land card, for example), its mana value is 0. Such cards can always be put onto the battlefield with Genesis Wave."},{"date":"2024-11-08","text":"If you have fewer than X cards in your library, you reveal all of them."},{"date":"2024-11-08","text":"You don't have to put permanent cards revealed this way onto the battlefield if you choose not to, regardless of their mana values."}],"subtypes":[],"supertypes":[],"text":"Reveal the top X cards of your library. You may put any number of permanent cards with mana value X or less from among them onto the battlefield. Then put all cards revealed this way that weren't put onto the battlefield into your graveyard.","type":"Sorcery","types":["Sorcery"]}"####,
    r####"{"colorIdentity":["R"],"colors":["R"],"convertedManaCost":3.0,"edhrecRank":11020,"edhrecSaltiness":0.6,"foreignData":[{"language":"French","name":"Directive de Saheeli","skuIds":{"nonfoil":"51b96994-b059-55ad-801f-4700ff758a5e"},"text":"Improvisation (Chaque artefact que vous engagez apr\u00e8s avoir activ\u00e9 des capacit\u00e9s de mana paie {1}.)\nR\u00e9v\u00e9lez les X cartes du dessus de votre biblioth\u00e8que. Parmi elles, vous pouvez mettre sur le champ de bataille n'importe quel nombre de cartes d'artefact avec un co\u00fbt converti de mana inf\u00e9rieur ou \u00e9gal \u00e0 X. Mettez ensuite dans votre cimeti\u00e8re toutes les cartes r\u00e9v\u00e9l\u00e9es de cette mani\u00e8re qui n'ont pas \u00e9t\u00e9 mises sur le champ de bataille.","type":"Rituel"},{"language":"German","name":"Saheelis Direktive","skuIds":{"nonfoil":"8be823a2-fae6-5246-b7e0-a16042dd7e02"},"text":"Improvisieren (Mit jedem Artefakt, das du tappst, nachdem du mit dem Aktivieren deiner Manaf\u00e4higkeiten fertig bist, bezahlst du {1}.)\nDecke die obersten X Karten deiner Bibliothek auf. Du kannst davon eine beliebige Anzahl an Artefaktkarten mit umgewandelten Manakosten von X oder weniger ins Spiel bringen. Lege dann alle Karten, die auf diese Weise aufgedeckt und nicht ins Spiel gebracht wurden, auf deinen Friedhof.","type":"Hexerei"},{"language":"Italian","name":"Direttiva di Saheeli","skuIds":{"nonfoil":"64526b54-86e9-5580-8364-25ae59901b13"},"text":"Improvvisare (Ogni artefatto che TAPpi dopo aver attivato le abilit\u00e0 di mana corrisponde al pagamento di {1}.)\nRivela le prime X carte del tuo grimorio. Puoi mettere sul campo di battaglia un qualsiasi numero di carte artefatto con costo di mana convertito pari o inferiore a X scelte tra esse. Poi metti nel tuo cimitero tutte le carte rivelate in questo modo che non sono state messe sul campo di battaglia.","type":"Stregoneria"},{"language":"Japanese","name":"\u30b5\u30d2\u30fc\u30ea\u306e\u6307\u4ee4","skuIds":{"nonfoil":"c2541f95-e895-5da3-9a07-38eaca0b3823"},"text":"\u5373\u5e2d\uff08\u3042\u306a\u305f\u306f\u3042\u306a\u305f\u306e\u30a2\u30fc\u30c6\u30a3\u30d5\u30a1\u30af\u30c8\u3092\u30bf\u30c3\u30d7\u3057\u3066\u3001\uff11\u500b\u3042\u305f\u308a{1}\u306e\u652f\u6255\u3044\u306b\u4ee3\u3048\u3066\u3082\u3088\u3044\u3002\uff09\n\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0a\u304b\u3089\uff38\u679a\u306e\u30ab\u30fc\u30c9\u3092\u516c\u958b\u3059\u308b\u3002\u3042\u306a\u305f\u306f\u305d\u306e\u4e2d\u304b\u3089\u3001\u70b9\u6570\u3067\u898b\u305f\u30de\u30ca\u30fb\u30b3\u30b9\u30c8\u304c\uff38\u4ee5\u4e0b\u3067\u3042\u308b\u30a2\u30fc\u30c6\u30a3\u30d5\u30a1\u30af\u30c8\u30fb\u30ab\u30fc\u30c9\u671b\u3080\u679a\u6570\u3092\u6226\u5834\u306b\u51fa\u3057\u3066\u3082\u3088\u3044\u3002\u305d\u306e\u5f8c\u3001\u3053\u308c\u306b\u3088\u308a\u516c\u958b\u3057\u6226\u5834\u306b\u51fa\u3055\u306a\u304b\u3063\u305f\u30ab\u30fc\u30c9\u3092\u3059\u3079\u3066\u3042\u306a\u305f\u306e\u5893\u5730\u306b\u7f6e\u304f\u3002","type":"\u30bd\u30fc\u30b5\u30ea\u30fc"},{"language":"Portuguese (Brazil)","name":"Diretriz de Saheeli","skuIds":{"nonfoil":"484dc66a-5264-52b8-9b85-c77087dff1b9"},"text":"Improvisar (Cada artefato que voc\u00ea vira ap\u00f3s terminar de ativar habilidades de mana paga por {1}.)\nRevele os X cards do topo de seu grim\u00f3rio. Voc\u00ea pode colocar qualquer n\u00famero de cards de artefato com custo de mana convertido igual ou inferior a X dentre eles no campo de batalha. Depois, coloque em seu cemit\u00e9rio todos os cards revelados dessa maneira que n\u00e3o foram colocados no campo de batalha.","type":"Feiti\u00e7o"},{"language":"Spanish","name":"Directiva de Saheeli","skuIds":{"nonfoil":"e06c60cc-82fb-525e-9502-1d108de17653"},"text":"Improvisar. (Cada artefacto que gires una vez que termines de activar habilidades de man\u00e1 paga {1}.)\nMuestra las X primeras cartas de tu biblioteca. Puedes poner en el campo de batalla cualquier cantidad de cartas de artefacto con coste de man\u00e1 convertido de X o menos que se encuentren entre ellas. Luego, pon en tu cementerio todas las cartas mostradas de esta manera que no fueron puestas en el campo de batalla.","type":"Conjuro"}],"identifiers":{"scryfallOracleId":"1aee3a93-d064-4d10-9fe6-879a1f342c1f"},"keywords":["Improvise"],"layout":"normal","legalities":{"commander":"Legal","duel":"Legal","legacy":"Legal","oathbreaker":"Legal","vintage":"Legal"},"manaCost":"{X}{R}{R}{R}","manaValue":3.0,"name":"Saheeli's Directive","printings":["C18","PZ2"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/c2fadb3bb3c648ae","cardmarket":"https://mtgjson.com/links/d8f0a7ab65e8f8bb","tcgplayer":"https://mtgjson.com/links/b49a31fab5cae4cc"},"rulings":[{"date":"2018-07-13","text":"All of the artifacts put onto the battlefield this way enter at the same time. If any have triggered abilities that trigger on something else entering the battlefield, they'll see each other."},{"date":"2018-07-13","text":"Equipment attached to a creature doesn't become tapped when that creature becomes tapped, and tapping that Equipment doesn't cause the creature to become tapped."},{"date":"2018-07-13","text":"If a card in your library has {X} in its mana cost, X is considered to be 0."},{"date":"2018-07-13","text":"If an artifact you control has a mana ability with {T} in the cost, activating that ability while casting a spell with improvise will result in the artifact being tapped when you pay the spell's costs. You won't be able to tap it again for improvise. Similarly, if you sacrifice an artifact to activate a mana ability while casting a spell with improvise, that artifact won't be on the battlefield when you pay the spell's costs, so you won't be able to tap it for improvise."},{"date":"2018-07-13","text":"Improvise can't be used to pay for anything other than the cost of casting the spell. For example, it can't be used during the resolution of an ability that says \"Counter target spell unless its controller pays {3}.\""},{"date":"2018-07-13","text":"Improvise can't pay for {W}, {U}, {B}, {R}, {G}, or {C} mana symbols in a spell's total cost."},{"date":"2018-07-13","text":"Improvise doesn't change a spell's mana cost or mana value."},{"date":"2018-07-13","text":"Saheeli's Directive considers the individual mana values of the artifact cards you reveal. The sum of their mana values may be greater than X."},{"date":"2018-07-13","text":"Tapping an artifact won't cause its abilities to stop applying unless those abilities say so."},{"date":"2018-07-13","text":"When calculating a spell's total cost, include any alternative costs, additional costs, or anything else that increases or reduces the cost to cast the spell. Improvise applies after the total cost is calculated."},{"date":"2018-07-13","text":"When using improvise to cast a spell with {X} in its mana cost, first choose the value for X. That choice, plus any cost increases or decreases, will determine the spell's total cost. Then you can tap artifacts you control to help pay that cost. For example, if you cast Saheeli's Directive and choose X to be 3, the total cost is {3}{R}{R}{R}. If you tap two artifacts, you'll have to pay {1}{R}{R}{R}."}],"subtypes":[],"supertypes":[],"text":"Improvise (Each artifact you tap after you're done activating mana abilities pays for {1}.)\nReveal the top X cards of your library. You may put any number of artifact cards with mana value X or less from among them onto the battlefield. Then put all cards revealed this way that weren't put onto the battlefield into your graveyard.","type":"Sorcery","types":["Sorcery"]}"####,
    r####"{"colorIdentity":["G"],"colors":["G"],"convertedManaCost":4.0,"edhrecRank":12553,"edhrecSaltiness":0.4,"foreignData":[{"language":"Chinese Simplified","name":"\u5f81\u53ec\u519b\u4f0d","skuIds":{"foil":"1ea53a16-01a5-57f7-9bf5-b7b34879aa50","nonfoil":"62ee8324-163c-5e9d-818d-d4ae1773acf9"},"text":"\u68c0\u89c6\u4f60\u724c\u5e93\u9876\u7684\u516d\u5f20\u724c\u3002\u5c06\u5176\u4e2d\u81f3\u591a\u4e24\u5f20\u603b\u6cd5\u672f\u529b\u7b49\u4e8e\u6216\u5c0f\u4e8e3\u7684\u751f\u7269\u724c\u653e\u8fdb\u6218\u573a\u3002\u5c06\u5176\u4f59\u7684\u724c\u4ee5\u4efb\u610f\u987a\u5e8f\u7f6e\u4e8e\u4f60\u724c\u5e93\u5e95\u3002","type":"\u77ac\u95f4"},{"language":"Chinese Traditional","name":"\u5fb5\u53ec\u8ecd\u4f0d","skuIds":{"foil":"8dab4027-511a-52a2-a338-9fbdb22f5cb2","nonfoil":"55055f2a-60aa-55be-884a-b9d9f0de2490"},"text":"\u6aa2\u8996\u4f60\u724c\u5eab\u9802\u7684\u516d\u5f35\u724c\u3002\u5c07\u5176\u4e2d\u81f3\u591a\u5169\u5f35\u7e3d\u9b54\u6cd5\u529b\u7b49\u65bc\u6216\u5c0f\u65bc3\u7684\u751f\u7269\u724c\u653e\u9032\u6230\u5834\u3002\u5c07\u5176\u9918\u7684\u724c\u4ee5\u4efb\u610f\u9806\u5e8f\u7f6e\u65bc\u4f60\u724c\u5eab\u5e95\u3002","type":"\u77ac\u9593"},{"language":"French","name":"Compagnie rassembl\u00e9e","skuIds":{"foil":"6c2df602-92e0-57af-8129-df76d15da11f","nonfoil":"ea24678a-6cd7-5ae3-9125-5a43efcc0f73"},"text":"Regardez les six cartes du dessus de votre biblioth\u00e8que. Parmi elles, mettez sur le champ de bataille jusqu'\u00e0 deux cartes de cr\u00e9ature avec un co\u00fbt converti de mana inf\u00e9rieur ou \u00e9gal \u00e0 3. Mettez le reste au-dessous de votre biblioth\u00e8que, dans l'ordre de votre choix.","type":"\u00c9ph\u00e9m\u00e8re"},{"language":"German","name":"Truppensammlung","skuIds":{"foil":"cc3e8af8-9663-5593-8b6f-e8a2487f9a56","nonfoil":"9e7c36d8-5b5a-5e8a-946d-5c183513f12d"},"text":"Schaue dir die obersten sechs Karten deiner Bibliothek an. Bringe davon bis zu zwei Kreaturenkarten mit umgewandelten Manakosten von 3 oder weniger ins Spiel. Lege den Rest in beliebiger Reihenfolge unter deine Bibliothek.","type":"Spontanzauber"},{"language":"Italian","name":"Compagnia a Raccolta","skuIds":{"foil":"ad02bb09-a39e-5c5a-856a-fd125bd5b3f3","nonfoil":"048c7be7-b74e-5409-8b04-c691e7a1d642"},"text":"Guarda le prime sei carte del tuo grimorio. Metti sul campo di battaglia fino a due carte creatura con costo di mana convertito pari o inferiore a 3 scelte tra esse. Metti le altre in fondo al tuo grimorio in qualsiasi ordine.","type":"Istantaneo"},{"language":"Japanese","name":"\u96c6\u5408\u3057\u305f\u4e2d\u968a","skuIds":{"foil":"65df883f-32c0-5e74-96a5-01dba61b80db","nonfoil":"846e60fa-69a1-5f56-a7db-8c8367722931"},"text":"\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0a\u304b\u3089\uff16\u679a\u306e\u30ab\u30fc\u30c9\u3092\u898b\u308b\u3002\u305d\u306e\u4e2d\u304b\u3089\u3001\u70b9\u6570\u3067\u898b\u305f\u30de\u30ca\u30fb\u30b3\u30b9\u30c8\u304c\uff13\u4ee5\u4e0b\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u30fb\u30ab\u30fc\u30c9\u3092\u6700\u5927\uff12\u679a\u307e\u3067\u6226\u5834\u306b\u51fa\u3059\u3002\u6b8b\u308a\u3092\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0b\u306b\u671b\u3080\u9806\u756a\u3067\u7f6e\u304f\u3002","type":"\u30a4\u30f3\u30b9\u30bf\u30f3\u30c8"},{"language":"Korean","name":"\uc120\ud0dd\ub41c \ubd80\ub300","skuIds":{"foil":"9e8f69cb-470a-5a0e-ae63-9ee877709233","nonfoil":"25bd7e2e-fa39-5984-a4b5-fa9abda85ed2"},"text":"\ub2f9\uc2e0\uc758 \uc11c\uace0 \ub9e8 \uc704\uc758 \uce74\ub4dc \uc5ec\uc12f \uc7a5\uc744 \ubcf8\ub2e4. \uadf8 \uce74\ub4dc\ub4e4 \uc911\uc5d0\uc11c \uc804\ud658\ub9c8\ub098\ube44\uc6a9\uc774 3 \uc774\ud558\uc778 \uc0dd\ubb3c \uce74\ub4dc\ub97c \ucd5c\ub300 \ub450 \uc7a5\uae4c\uc9c0 \uc804\uc7a5\uc5d0 \ub193\ub294\ub2e4. \ub098\uba38\uc9c0 \uce74\ub4dc\ub4e4\uc740 \ub2f9\uc2e0\uc774 \uc6d0\ud558\ub294 \uc21c\uc11c\ub85c \ub2f9\uc2e0\uc758 \uc11c\uace0 \ub9e8 \ubc11\uc5d0 \ub193\ub294\ub2e4.","type":"\uc21c\uac04\ub9c8\ubc95"},{"language":"Portuguese (Brazil)","name":"Companhia Agrupada","skuIds":{"foil":"42f7b3b5-2092-595a-ada9-5a7d48d863e0","nonfoil":"fb5e8e91-52ec-541b-abc3-a18583d0ebaa"},"text":"Olhe os seis cards do topo de seu grim\u00f3rio. Coloque at\u00e9 dois cards de criatura com custo de mana convertido igual ou inferior a 3 dentre eles no campo de batalha. Coloque o restante no fundo de seu grim\u00f3rio em qualquer ordem.","type":"M\u00e1gica Instant\u00e2nea"},{"language":"Russian","name":"\u0421\u0431\u043e\u0440\u043d\u0430\u044f \u0420\u043e\u0442\u0430","skuIds":{"foil":"b8b8b077-8873-561a-be2b-45de2bd4a1cc","nonfoil":"a2f941fa-3561-50ef-a3e0-ca9478471583"},"text":"\u041f\u043e\u0441\u043c\u043e\u0442\u0440\u0438\u0442\u0435 \u0448\u0435\u0441\u0442\u044c \u0432\u0435\u0440\u0445\u043d\u0438\u0445 \u043a\u0430\u0440\u0442 \u0432\u0430\u0448\u0435\u0439 \u0431\u0438\u0431\u043b\u0438\u043e\u0442\u0435\u043a\u0438. \u041f\u043e\u043b\u043e\u0436\u0438\u0442\u0435 \u0438\u0437 \u043d\u0438\u0445 \u043d\u0430 \u043f\u043e\u043b\u0435 \u0431\u0438\u0442\u0432\u044b \u043d\u0435 \u0431\u043e\u043b\u0435\u0435 \u0434\u0432\u0443\u0445 \u043a\u0430\u0440\u0442 \u0441\u0443\u0449\u0435\u0441\u0442\u0432 \u0441 \u043a\u043e\u043d\u0432\u0435\u0440\u0442\u0438\u0440\u043e\u0432\u0430\u043d\u043d\u043e\u0439 \u043c\u0430\u043d\u0430-\u0441\u0442\u043e\u0438\u043c\u043e\u0441\u0442\u044c\u044e 3 \u0438\u043b\u0438 \u043c\u0435\u043d\u044c\u0448\u0435. \u041f\u043e\u043b\u043e\u0436\u0438\u0442\u0435 \u043e\u0441\u0442\u0430\u043b\u044c\u043d\u044b\u0435 \u043a\u0430\u0440\u0442\u044b \u0432 \u043d\u0438\u0437 \u0432\u0430\u0448\u0435\u0439 \u0431\u0438\u0431\u043b\u0438\u043e\u0442\u0435\u043a\u0438 \u0432 \u043b\u044e\u0431\u043e\u043c \u043f\u043e\u0440\u044f\u0434\u043a\u0435.","type":"\u041c\u0433\u043d\u043e\u0432\u0435\u043d\u043d\u043e\u0435 \u0437\u0430\u043a\u043b\u0438\u043d\u0430\u043d\u0438\u0435"},{"language":"Spanish","name":"Compa\u00f1\u00eda reunida","skuIds":{"foil":"db4e0de5-bc45-5fa3-88f1-74ea28e6e1e4","nonfoil":"2ef49ac0-38ca-580f-a82a-6c0afae7ba77"},"text":"Mira las seis primeras cartas de tu biblioteca. Pon en el campo de batalla hasta dos cartas de criatura que se encuentren entre ellas con coste de man\u00e1 convertido de 3 o menos. Pon el resto en el fondo de tu biblioteca en cualquier orden.","type":"Instant\u00e1neo"}],"identifiers":{"scryfallOracleId":"0bbcb563-09b3-499d-a9cc-3a6d7501dd81"},"layout":"normal","legalities":{"brawl":"Legal","commander":"Legal","duel":"Legal","gladiator":"Legal","historic":"Legal","legacy":"Legal","modern":"Legal","oathbreaker":"Legal","pioneer":"Legal","timeless":"Legal","vintage":"Legal"},"manaCost":"{3}{G}","manaValue":4.0,"name":"Collected Company","printings":["AKR","DTK","MB2","PW21","SLD","SPG"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/487c47e535c6591a","cardKingdomFoil":"https://mtgjson.com/links/f9f23db029a4369c","cardmarket":"https://mtgjson.com/links/e85cce440c3a9296","cardmarketFoil":"https://mtgjson.com/links/efe40c4d82dc2e39","tcgplayer":"https://mtgjson.com/links/dfa72450fd314dd8"},"rulings":[{"date":"2015-02-25","text":"Each of the creature cards can have mana value 3 or less. It's not the total mana value of the two cards."}],"subtypes":[],"supertypes":[],"text":"Look at the top six cards of your library. Put up to two creature cards with mana value 3 or less from among them onto the battlefield. Put the rest on the bottom of your library in any order.","type":"Instant","types":["Instant"]}"####,
    r####"{"colorIdentity":["B","U"],"colors":["B","U"],"convertedManaCost":2.0,"edhrecRank":9205,"edhrecSaltiness":0.26,"foreignData":[{"language":"French","name":"Nashi, chercheur de l'ombre","skuIds":{"foil":"d1b27525-5214-5ebe-8a54-da6fc4342b39","nonfoil":"b0fc52d6-6920-51bb-bcb8-6df9e573f482"},"text":"Menace\n\u00c0 chaque fois que Nashi, chercheur de l'ombre inflige des blessures de combat \u00e0 un joueur, vous meulez autant de cartes. Vous pouvez mettre n'importe quel nombre de cartes l\u00e9gendaires et/ou d'enchantement parmi elles dans votre main. Si vous n'avez pas mis de carte dans votre main de cette mani\u00e8re, mettez un marqueur +1/+1 sur Nashi.","type":"Cr\u00e9ature l\u00e9gendaire \u2014 rat et ninja et sorcier"},{"language":"German","name":"Nashi, Suchender im Dunkeln","skuIds":{"foil":"8265e257-e7c2-591a-aa56-a553146508b2","nonfoil":"6856c0cd-dd66-5d72-b86a-26634dbe9e34"},"text":"Bedrohlich\nImmer wenn Nashi, Suchender im Dunkeln, einem Spieler Kampfschaden zuf\u00fcgt, millst du entsprechend viele Karten. Du kannst davon eine beliebige Anzahl an legend\u00e4ren Karten und/oder Verzauberungskarten auf deine Hand nehmen. Falls du auf diese Weise keine Karten auf deine Hand nimmst, lege eine +1/+1-Marke auf Nashi.","type":"Legend\u00e4re Kreatur \u2014 Ratte, Ninja, Zauberer"},{"language":"Italian","name":"Nashi, Esploratore nell'Oscurit\u00e0","skuIds":{"foil":"147e7496-650e-51d3-b36a-bd6e9d71cdbb","nonfoil":"7ea64157-0233-5e92-b8ee-7e49dac2364f"},"text":"Minacciare\nOgniqualvolta Nashi, Esploratore nell'Oscurit\u00e0 infligge danno da combattimento a un giocatore, macini altrettante carte. Puoi aggiungere alla tua mano un qualsiasi numero di carte leggendarie e/o incantesimo scelte tra esse. Se non aggiungi carte alla tua mano in questo modo, metti un segnalino +1/+1 su Nashi.","type":"Creatura Leggendaria \u2014 Mago Ninja Ratto"},{"language":"Japanese","name":"\u95c7\u306e\u4e2d\u306e\u7814\u7a76\u8005\u3001\u30ca\u30b7","skuIds":{"foil":"881319d3-cfb9-56b6-b624-943e141136aa","nonfoil":"004651ea-00c7-597e-bb8a-58eec26aabf0"},"text":"\u5a01\u8feb\n\u95c7\u306e\u4e2d\u306e\u7814\u7a76\u8005\u3001\u30ca\u30b7\u304c\u30d7\u30ec\u30a4\u30e4\u30fc\uff11\u4eba\u306b\u6226\u95d8\u30c0\u30e1\u30fc\u30b8\u3092\u4e0e\u3048\u308b\u305f\u3073\u3001\u3042\u306a\u305f\u306f\u305d\u306e\u70b9\u6570\u306b\u7b49\u3057\u3044\u679a\u6570\u306e\u30ab\u30fc\u30c9\u3092\u5207\u524a\u3059\u308b\u3002\u305d\u306e\u4e2d\u304b\u3089\u3001\u4f1d\u8aac\u3084\u30a8\u30f3\u30c1\u30e3\u30f3\u30c8\u3067\u3042\u308b\u671b\u3080\u679a\u6570\u306e\u30ab\u30fc\u30c9\u3092\u3042\u306a\u305f\u306e\u624b\u672d\u306b\u52a0\u3048\u3066\u3082\u3088\u3044\u3002\u3053\u308c\u306b\u3088\u308a\u30ab\u30fc\u30c9\u3092\u3042\u306a\u305f\u306e\u624b\u672d\u306b\u52a0\u3048\u306a\u304b\u3063\u305f\u306a\u3089\u3001\u3053\u308c\u306e\u4e0a\u306b\uff0b\uff11/\uff0b\uff11\u30ab\u30a6\u30f3\u30bf\u30fc\uff11\u500b\u3092\u7f6e\u304f\u3002","type":"\u4f1d\u8aac\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc \u2014 \u30cd\u30ba\u30df\u30fb\u5fcd\u8005\u30fb\u30a6\u30a3\u30b6\u30fc\u30c9"},{"language":"Spanish","name":"Nashi, buscador en la oscuridad","skuIds":{"foil":"3aa62bde-2533-53fa-a145-1fac61dd0cc1","nonfoil":"a6b47b46-beca-5b70-9526-1e0db168ac3c"},"text":"Amenaza.\nSiempre que Nashi, buscador en la oscuridad haga da\u00f1o de combate a un jugador, mueles esa misma cantidad de cartas. Puedes poner en tu mano cualquier cantidad de cartas legendarias y/o de encantamiento de entre ellas. Si no pusiste cartas en tu mano de esta manera, pon un contador +1/+1 sobre Nashi.","type":"Criatura legendaria \u2014 Hechicero ninja rata"}],"identifiers":{"scryfallOracleId":"ade8fc6f-efaa-4a24-96d8-99ef5026cd74"},"keywords":["Menace","Mill"],"layout":"normal","leadershipSkills":{"brawl":true,"commander":true,"oathbreaker":false,"pauper_commander":false,"predh":false},"legalities":{"alchemy":"Legal","brawl":"Legal","commander":"Legal","duel":"Legal","future":"Legal","gladiator":"Legal","historic":"Legal","legacy":"Legal","modern":"Legal","oathbreaker":"Legal","penny":"Legal","pioneer":"Legal","standard":"Legal","standardbrawl":"Legal","timeless":"Legal","vintage":"Legal"},"manaCost":"{U}{B}","manaValue":2.0,"name":"Nashi, Searcher in the Dark","power":"2","printings":["DSK","PDSK"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/ad7ca996bc36d426","cardKingdomFoil":"https://mtgjson.com/links/b576c46e6d1b8f3c","cardmarket":"https://mtgjson.com/links/ee7ee0098d64d1c0","cardmarketFoil":"https://mtgjson.com/links/0942796f2527c558","tcgplayer":"https://mtgjson.com/links/6a7e933fc69c147e"},"subtypes":["Rat","Ninja","Wizard"],"supertypes":["Legendary"],"text":"Menace\nWhenever Nashi deals combat damage to a player, you mill that many cards. You may put any number of legendary and/or enchantment cards from among them into your hand. If you put no cards into your hand this way, put a +1/+1 counter on Nashi.","toughness":"2","type":"Legendary Creature \u2014 Rat Ninja Wizard","types":["Creature"]}"####,
    r####"{"colorIdentity":["G"],"colors":["G"],"convertedManaCost":4.0,"edhrecRank":8766,"edhrecSaltiness":0.05,"foreignData":[{"language":"Chinese Simplified","name":"\u884c\u730e\u91ce\u5730","skuIds":{"foil":"e90073da-58f1-59f2-8edd-21c237bb2d94","nonfoil":"6d5cd8a2-e232-5589-9a12-1d849d48c84f"},"text":"\u589e\u5e45{3}{G}\uff08\u4f60\u4f7f\u7528\u6b64\u5492\u8bed\u65f6\u53ef\u4ee5\u989d\u5916\u652f\u4ed8{3}{G}\u3002)\n\u4ece\u4f60\u7684\u724c\u5e93\u4e2d\u641c\u5bfb\u81f3\u591a\u4e24\u5f20\u6811\u6797\u724c\uff0c\u5e76\u5c06\u5b83\u4eec\u6a2a\u7f6e\u8fdb\u573a\u3002 \u7136\u540e\u5c06\u4f60\u7684\u724c\u5e93\u6d17\u724c\u3002\n\u5982\u679c\u66fe\u652f\u4ed8\u5176\u589e\u5e45\u8d39\u7528\uff0c\u5219\u91cd\u7f6e\u6240\u6709\u4ee5\u6b64\u6cd5\u653e\u7f6e\u8fdb\u573a\u7684\u6811\u6797\u3002 \u5b83\u4eec\u6210\u4e3a3/3\u7eff\u8272\uff0c\u5177\u654f\u6377\u5f02\u80fd\u7684\u751f\u7269\uff0c\u5e76\u4e14\u4ecd\u7136\u662f\u5730\u3002","type":"\u6cd5\u672f"},{"language":"French","name":"\u00c9tendues de chasse","skuIds":{"foil":"e8df5ef3-9527-5efa-9a8c-ed5736230358","nonfoil":"124a9bf5-9ab8-5fa8-971b-73ff1a3b03d9"},"text":"Kick {3}{G} (Vous pouvez payer {3}{G} suppl\u00e9mentaires au moment o\u00f9 vous jouez ce sort.)\nCherchez dans votre biblioth\u00e8que jusqu'\u00e0 deux cartes de for\u00eat et mettez-les en jeu engag\u00e9es. M\u00e9langez ensuite votre biblioth\u00e8que.\nSi le co\u00fbt de kick a \u00e9t\u00e9 pay\u00e9, d\u00e9gagez toutes les for\u00eats mises en jeu de cette mani\u00e8re. Elles deviennent des cr\u00e9atures 3/3 vertes avec la c\u00e9l\u00e9rit\u00e9 qui sont toujours des terrains.","type":"Rituel"},{"language":"German","name":"Jagende Wildnis","skuIds":{"foil":"8d5cc381-2fe7-5afb-92bb-7c72e660802f","nonfoil":"3cca0176-6623-5691-8b11-6d2ccaf9669d"},"text":"Bonus {3}{G} (Du kannst zus\u00e4tzlich {3}{G} bezahlen, sowie du diesen Zauberspruch spielst.)\nDurchsuche deine Bibliothek nach bis zu zwei W\u00e4ldern und bringe sie getappt ins Spiel. Mische danach deine Bibliothek.\nWurden die Bonuskosten bezahlt, enttappe alle W\u00e4lder, die auf diese Weise ins Spiel gebracht wurden. Sie werden zu 3/3 gr\u00fcnen Kreaturen mit Eile, die auch noch L\u00e4nder sind.","type":"Hexerei"},{"language":"Italian","name":"Terreni di Caccia","skuIds":{"foil":"68b409a3-7c9f-5750-ac28-b252dd479983","nonfoil":"ffd4df08-b52d-59a4-ac8a-fc61857fa502"},"text":"Potenziamento {3}{G} (Puoi spendere {3}{G} addizionale quando giochi questa magia.)\nPassa in rassegna il tuo grimorio per trovare fino a due carte Foresta e mettile in gioco TAPpate. Poi rimescola il tuo grimorio.\nSe \u00e8 stato pagato il costo di potenziamento, STAPpa tutte le Foreste messe in gioco in questo modo. Diventano creature 3/3 verdi con rapidit\u00e0 che sono ancora terre.","type":"Stregoneria"},{"language":"Japanese","name":"\u672a\u958b\u306e\u72e9\u308a","skuIds":{"foil":"0212f71a-0d23-5b96-861f-71a3720393a6","nonfoil":"2d4e66cf-5065-5ab8-8e3b-83c17ee102a5"},"text":"\u30ad\u30c3\u30ab\u30fc {3}{G} \uff08\u3042\u306a\u305f\u306f\u3053\u306e\u546a\u6587\u3092\u30d7\u30ec\u30a4\u3059\u308b\u306b\u969b\u3057\u3001\u8ffd\u52a0\u3067{3}{G}\u3092\u652f\u6255\u3063\u3066\u3082\u3088\u3044\u3002\uff09\n\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u304b\u3089\u68ee\u30ab\u30fc\u30c9\u3092\u6700\u5927\uff12\u679a\u307e\u3067\u63a2\u3057\u3001\u305d\u308c\u3089\u3092\u30bf\u30c3\u30d7\u72b6\u614b\u3067\u5834\u306b\u51fa\u3059\u3002 \u305d\u306e\u5f8c\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u3092\u5207\u308a\u76f4\u3059\u3002\n\u30ad\u30c3\u30ab\u30fc\u30b3\u30b9\u30c8\u304c\u652f\u6255\u308f\u308c\u3066\u3044\u308b\u5834\u5408\u3001\u3053\u308c\u306b\u3088\u308a\u5834\u306b\u51fa\u305f\u3059\u3079\u3066\u306e\u68ee\u3092\u30a2\u30f3\u30bf\u30c3\u30d7\u3059\u308b\u3002 \u305d\u308c\u3089\u306f\u901f\u653b\u3092\u6301\u3064\u7dd1\u306e\uff13/\uff13\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u306b\u306a\u308b\u3002\u305d\u308c\u3089\u306f\u571f\u5730\u3067\u3082\u3042\u308b\u3002","type":"\u30bd\u30fc\u30b5\u30ea\u30fc"},{"language":"Portuguese (Brazil)","name":"Regi\u00f5es de Ca\u00e7a","skuIds":{"foil":"a072e024-394c-5578-93ce-5f67f6587472","nonfoil":"d2d34349-38bb-593d-ac28-26a829bc91db"},"text":"Refor\u00e7ar {3}{G} (Voc\u00ea pode pagar um custo adicional de {3}{G} ao jogar esta m\u00e1gica.)\nProcure em seu grim\u00f3rio por at\u00e9 dois cards de Floresta e coloque-os em jogo virados. Depois, embaralhe seu grim\u00f3rio.\nSe o custo de refor\u00e7o tiver sido pago, desvire todas as Florestas colocadas em jogo desta maneira. Elas se tornam criaturas 3/3 verdes com \u00edmpeto que ainda s\u00e3o terrenos.","type":"Feiti\u00e7o"},{"language":"Russian","name":"\u041e\u0445\u043e\u0442\u044f\u0449\u0438\u0435\u0441\u044f \u041f\u0440\u043e\u0441\u0442\u043e\u0440\u044b","skuIds":{"foil":"84fc5405-c29c-564c-8c99-2749a653e787","nonfoil":"e67efa3c-0c78-556c-bfed-78473743d491"},"text":"\u0423\u0441\u0438\u043b\u0438\u0442\u0435\u043b\u044c {3}{G} (\u0412\u044b \u043c\u043e\u0436\u0435\u0442\u0435 \u0437\u0430\u043f\u043b\u0430\u0442\u0438\u0442\u044c \u0434\u043e\u043f\u043e\u043b\u043d\u0438\u0442\u0435\u043b\u044c\u043d\u043e {3}{G} \u0432\u043e \u0432\u0440\u0435\u043c\u044f \u0438\u0433\u0440\u044b \u044d\u0442\u043e\u0433\u043e \u0437\u0430\u043a\u043b\u0438\u043d\u0430\u043d\u0438\u044f.)\n\u041f\u043e\u0438\u0449\u0438\u0442\u0435 \u0432 \u0441\u0432\u043e\u0435\u0439 \u0431\u0438\u0431\u043b\u0438\u043e\u0442\u0435\u043a\u0435 \u0434\u043e \u0434\u0432\u0443\u0445 \u043a\u0430\u0440\u0442 \u041b\u0435\u0441\u043e\u0432 \u0438 \u043f\u043e\u043b\u043e\u0436\u0438\u0442\u0435 \u0438\u0445 \u043f\u043e\u0432\u0435\u0440\u043d\u0443\u0442\u044b\u043c\u0438 \u0432 \u0438\u0433\u0440\u0443. \u0417\u0430\u0442\u0435\u043c \u043f\u0435\u0440\u0435\u0442\u0430\u0441\u0443\u0439\u0442\u0435 \u0441\u0432\u043e\u044e \u0431\u0438\u0431\u043b\u0438\u043e\u0442\u0435\u043a\u0443.\n\u0415\u0441\u043b\u0438 \u0431\u044b\u043b\u0430 \u0437\u0430\u043f\u043b\u0430\u0447\u0435\u043d\u0430 \u0441\u0442\u043e\u0438\u043c\u043e\u0441\u0442\u044c \u0443\u0441\u0438\u043b\u0438\u0442\u0435\u043b\u044f, \u0440\u0430\u0437\u0432\u0435\u0440\u043d\u0438\u0442\u0435 \u0432\u0441\u0435 \u041b\u0435\u0441\u0430, \u043f\u043e\u043b\u043e\u0436\u0435\u043d\u043d\u044b\u0435 \u0432 \u0438\u0433\u0440\u0443 \u044d\u0442\u0438\u043c \u043f\u0443\u0442\u0435\u043c \u041e\u043d\u0438 \u0441\u0442\u0430\u043d\u043e\u0432\u044f\u0442\u0441\u044f \u0437\u0435\u043b\u0435\u043d\u044b\u043c\u0438 \u0441\u0443\u0449\u0435\u0441\u0442\u0432\u0430\u043c\u0438 3/3 \u0441 \u0423\u0441\u043a\u043e\u0440\u0435\u043d\u0438\u0435\u043c \u0438 \u043e\u0441\u0442\u0430\u044e\u0442\u0441\u044f \u0437\u0435\u043c\u043b\u044f\u043c\u0438.","type":"\u0412\u043e\u043b\u0448\u0435\u0431\u0441\u0442\u0432\u043e"},{"language":"Spanish","name":"Terreno de caza","skuIds":{"foil":"bb86ddf7-01d8-5378-bc24-29a8ce41ddfd","nonfoil":"4ff2227e-87ea-5e15-a672-4475450433fe"},"text":"Est\u00edmulo {3}{G} (Puedes pagar {3}{G} adicional en cuanto juegues este hechizo.)\nBusca en tu biblioteca hasta dos cartas de bosque y ponlas en juego giradas. Luego baraja tu biblioteca.\nSi se pag\u00f3 el coste de est\u00edmulo, endereza todos los bosques puestos en juego de esta manera. Se convierten en criaturas verdes 3/3 con la habilidad de prisa que siguen siendo tierras.","type":"Conjuro"}],"identifiers":{"scryfallOracleId":"e9bc324e-c200-45ef-8b9b-52a61d0f7374"},"keywords":["Kicker"],"layout":"normal","legalities":{"commander":"Legal","duel":"Legal","legacy":"Legal","modern":"Legal","oathbreaker":"Legal","predh":"Legal","vintage":"Legal"},"manaCost":"{3}{G}","manaValue":4.0,"name":"Hunting Wilds","printings":["C18","PLC"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/f0e58a32200c1f6e","cardKingdomFoil":"https://mtgjson.com/links/e05cbf1ef38207cf","cardmarket":"https://mtgjson.com/links/b955fd83be7b02cc","cardmarketFoil":"https://mtgjson.com/links/a967543e9794b4c3","tcgplayer":"https://mtgjson.com/links/0e3b207b424b7b9b"},"rulings":[{"date":"2007-02-01","text":"If the Hunting Wilds is kicked, the Forests become creatures with haste permanently."},{"date":"2009-10-01","text":"A noncreature permanent that turns into a creature is subject to the \"summoning sickness\" rule: It can only attack, and its {T} abilities can only be activated, if its controller has continuously controlled that permanent since the beginning of their most recent turn."},{"date":"2024-11-08","text":"If a card or token enters as a copy of a permanent, the new permanent isn't kicked, even if the original was."},{"date":"2024-11-08","text":"If a spell's kicker cost was paid, the spell is \"kicked.\""},{"date":"2024-11-08","text":"If you copy a kicked spell on the stack, the copy is also kicked. If the copied spell is a permanent spell, the token the copy of that spell becomes when it enters is also kicked."},{"date":"2024-11-08","text":"If you put a permanent with a kicker ability onto the battlefield without casting it, you can't kick it."},{"date":"2024-11-08","text":"The kicker ability doesn't let you pay a kicker cost more than once."},{"date":"2024-11-08","text":"To determine a spell's total cost, start with the mana cost (or an alternative cost if another card's effect allows you to pay one instead), add any cost increases (such as kicker), then apply any cost reductions. The spell's mana value remains unchanged, no matter what the total cost to cast it was."}],"subtypes":[],"supertypes":[],"text":"Kicker {3}{G} (You may pay an additional {3}{G} as you cast this spell.)\nSearch your library for up to two Forest cards, put them onto the battlefield tapped, then shuffle.\nIf this spell was kicked, untap all Forests put onto the battlefield this way. They become 3/3 green creatures with haste that are still lands.","type":"Sorcery","types":["Sorcery"]}"####,
    r####"{"colorIdentity":["U"],"colors":["U"],"convertedManaCost":3.0,"edhrecRank":17292,"edhrecSaltiness":0.19,"foreignData":[{"language":"Chinese Simplified","name":"\u8c1c\u9898\u62fc\u56fe","skuIds":{"foil":"df30ba4c-abce-51c5-bfa4-f10a53e24b7f","nonfoil":"b0142694-50ba-5a32-a648-6a64dc757bf1"},"text":"\u5c55\u793a\u4f60\u724c\u5e93\u9876\u7684\u4e94\u5f20\u724c\u3002\u5c06\u5176\u4e2d\u81f3\u591a\u4e24\u5f20\u77ac\u95f4\u548c\uff0f\u6216\u6cd5\u672f\u724c\u7f6e\u4e8e\u4f60\u624b\u4e0a\uff0c\u5176\u4f59\u5219\u7f6e\u5165\u4f60\u7684\u575f\u5893\u573a\u3002","type":"\u6cd5\u672f"},{"language":"Chinese Traditional","name":"\u8b0e\u984c\u62fc\u5716","skuIds":{"foil":"9df6c394-b2b3-55d8-9c41-8f7c5333e35d","nonfoil":"bdc37e8a-033d-510e-90ea-e90a676eec81"},"text":"\u5c55\u793a\u4f60\u724c\u5eab\u9802\u7684\u4e94\u5f35\u724c\u3002\u5c07\u5176\u4e2d\u81f3\u591a\u5169\u5f35\u77ac\u9593\u548c\uff0f\u6216\u5deb\u8853\u724c\u7f6e\u65bc\u4f60\u624b\u4e0a\uff0c\u5176\u9918\u5247\u7f6e\u5165\u4f60\u7684\u58b3\u5893\u5834\u3002","type":"\u5deb\u8853"},{"language":"French","name":"Composants de l'\u00e9nigme","skuIds":{"foil":"28c90208-b230-5370-8e14-0c6dbf042e50","nonfoil":"60af21b7-b612-550d-9014-029167c21f4e"},"text":"R\u00e9v\u00e9lez les cinq cartes du dessus de votre biblioth\u00e8que. Mettez jusqu'\u00e0 deux cartes d'\u00e9ph\u00e9m\u00e8re et/ou de rituel parmi elles dans votre main et le reste dans votre cimeti\u00e8re.","type":"Rituel"},{"language":"German","name":"Teile des Puzzles","skuIds":{"foil":"fbb28208-e869-5ca6-b99c-ed4a1a424f3c","nonfoil":"4630cd7a-f90c-5218-bc95-904dd5cf252f"},"text":"Decke die obersten f\u00fcnf Karten deiner Bibliothek auf. Nimm davon bis zu zwei Spontanzauber- und/oder Hexerei-Karten auf deine Hand und lege den Rest auf deinen Friedhof.","type":"Hexerei"},{"language":"Italian","name":"Pezzi dell'Enigma","skuIds":{"foil":"2fb3b66e-fbbb-5b90-b003-5ce35cb9b323","nonfoil":"1035427a-4d9c-5ecc-ba6d-f51659ef91c6"},"text":"Rivela le prime cinque carte del tuo grimorio. Aggiungi alla tua mano fino a due carte istantaneo e/o stregoneria scelte tra esse e metti le altre nel tuo cimitero.","type":"Stregoneria"},{"language":"Japanese","name":"\u30d1\u30ba\u30eb\u306e\u6b20\u7247","skuIds":{"foil":"cbe1bb6b-c6c0-5309-a4c5-a07d9c877b0a","nonfoil":"f17d3553-0468-5705-b720-bfeda665cfeb"},"text":"\u3042\u306a\u305f\u306e\u30e9\u30a4\u30d6\u30e9\u30ea\u30fc\u306e\u4e00\u756a\u4e0a\u304b\u3089\uff15\u679a\u306e\u30ab\u30fc\u30c9\u3092\u516c\u958b\u3059\u308b\u3002\u305d\u306e\u4e2d\u304b\u3089\u30a4\u30f3\u30b9\u30bf\u30f3\u30c8\u30fb\u30ab\u30fc\u30c9\u3084\u30bd\u30fc\u30b5\u30ea\u30fc\u30fb\u30ab\u30fc\u30c9\u6700\u5927\uff12\u679a\u3092\u3042\u306a\u305f\u306e\u624b\u672d\u306b\u52a0\u3048\u3001\u6b8b\u308a\u3092\u3042\u306a\u305f\u306e\u5893\u5730\u306b\u7f6e\u304f\u3002","type":"\u30bd\u30fc\u30b5\u30ea\u30fc"},{"language":"Korean","name":"\ud37c\uc990 \uc870\uac01","skuIds":{"foil":"5d5ebce9-a4d5-503e-85f0-73216d59b534","nonfoil":"5a11d3a3-e54e-533c-95c7-03fb9d6c3cdd"},"text":"\ub2f9\uc2e0\uc758 \uc11c\uace0 \ub9e8 \uc704\uc758 \uce74\ub4dc \ub2e4\uc12f \uc7a5\uc744 \uacf5\uac1c\ud55c\ub2e4. \uadf8 \uc911\uc5d0\uc11c \uc21c\uac04\ub9c8\ubc95 \ubc0f/\ub610\ub294 \uc9d1\uc911\ub9c8\ubc95 \uce74\ub4dc\ub97c \ucd5c\ub300 \ub450 \uc7a5\uae4c\uc9c0 \uc790\uc2e0\uc758 \uc190\uc73c\ub85c \uac00\uc838\uc624\uace0 \ub098\uba38\uc9c0 \uce74\ub4dc\ub294 \ubb34\ub364\uc5d0 \ub123\ub294\ub2e4.","type":"\uc9d1\uc911\ub9c8\ubc95"},{"language":"Portuguese (Brazil)","name":"Pe\u00e7as do Quebra-cabe\u00e7a","skuIds":{"foil":"6f16e5ce-f34b-5852-ab5b-22d3996da22e","nonfoil":"0ee363fb-6928-50c8-9d97-c931cfc1cbd1"},"text":"Revele os cinco cards do topo de seu grim\u00f3rio. Coloque at\u00e9 dois cards de m\u00e1gica instant\u00e2nea e/ou feiti\u00e7o dentre eles em sua m\u00e3o e o restante em seu cemit\u00e9rio.","type":"Feiti\u00e7o"},{"language":"Russian","name":"\u0424\u0440\u0430\u0433\u043c\u0435\u043d\u0442\u044b \u0413\u043e\u043b\u043e\u0432\u043e\u043b\u043e\u043c\u043a\u0438","skuIds":{"foil":"ab77da79-a308-5612-8625-687c420f77ee","nonfoil":"5a10767c-d766-53a3-8fdd-f1e7df8450d0"},"text":"\u041f\u043e\u043a\u0430\u0436\u0438\u0442\u0435 \u043f\u044f\u0442\u044c \u0432\u0435\u0440\u0445\u043d\u0438\u0445 \u043a\u0430\u0440\u0442 \u0432\u0430\u0448\u0435\u0439 \u0431\u0438\u0431\u043b\u0438\u043e\u0442\u0435\u043a\u0438. \u041f\u043e\u043b\u043e\u0436\u0438\u0442\u0435 \u043d\u0435 \u0431\u043e\u043b\u0435\u0435 \u0434\u0432\u0443\u0445 \u043d\u0430\u0445\u043e\u0434\u044f\u0449\u0438\u0445\u0441\u044f \u0441\u0440\u0435\u0434\u0438 \u043d\u0438\u0445 \u043a\u0430\u0440\u0442 \u043c\u0433\u043d\u043e\u0432\u0435\u043d\u043d\u043e\u0433\u043e \u0437\u0430\u043a\u043b\u0438\u043d\u0430\u043d\u0438\u044f \u0438 (\u0438\u043b\u0438) \u0432\u043e\u043b\u0448\u0435\u0431\u0441\u0442\u0432\u0430 \u0432 \u0432\u0430\u0448\u0443 \u0440\u0443\u043a\u0443, \u0430 \u043e\u0441\u0442\u0430\u043b\u044c\u043d\u044b\u0435 \u2014 \u043d\u0430 \u0432\u0430\u0448\u0435 \u043a\u043b\u0430\u0434\u0431\u0438\u0449\u0435.","type":"\u0412\u043e\u043b\u0448\u0435\u0431\u0441\u0442\u0432\u043e"},{"language":"Spanish","name":"Piezas del rompecabezas","skuIds":{"foil":"55135a65-b13b-5685-a23b-8cde9a5ad021","nonfoil":"5caad885-b863-5d5d-874a-e6f3e7548a79"},"text":"Muestra las primeras cinco cartas de tu biblioteca. Pon en tu mano hasta dos cartas de instant\u00e1neo o de conjuro que se encuentren entre ellas y pon el resto en tu cementerio.","type":"Conjuro"}],"identifiers":{"scryfallOracleId":"7b599b4d-95bc-4ed2-a167-84a697944747"},"layout":"normal","legalities":{"brawl":"Legal","commander":"Legal","duel":"Legal","gladiator":"Legal","historic":"Legal","legacy":"Legal","modern":"Legal","oathbreaker":"Legal","pauper":"Legal","paupercommander":"Legal","penny":"Legal","pioneer":"Legal","timeless":"Legal","vintage":"Legal"},"manaCost":"{2}{U}","manaValue":3.0,"name":"Pieces of the Puzzle","printings":["SIR","SOI"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/bf9df3feaf1dc930","cardKingdomFoil":"https://mtgjson.com/links/660fc3bbcc2694da","cardmarket":"https://mtgjson.com/links/eba448a2eb90208a","cardmarketFoil":"https://mtgjson.com/links/63bb9755c31a2508","tcgplayer":"https://mtgjson.com/links/9151d99a6b89dd10"},"subtypes":[],"supertypes":[],"text":"Reveal the top five cards of your library. Put up to two instant and/or sorcery cards from among them into your hand and the rest into your graveyard.","type":"Sorcery","types":["Sorcery"]}"####,
];
