use std::sync::Arc;

use engine::game::deck_loading::DeckEntry;
use engine::game::derived_views::{
    derive_filtered_views, derive_views, ClientGameStateRef, DerivedViews,
};
use engine::game::effects::choose;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{ChoiceType, ChosenSubtypeKind};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{GameState, OutsideGameCardUse, PlayerDeckPool, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text checked against data/mtgjson/AtomicCards.json.
const CAVERN: &str = "As this land enters, choose a creature type.\n{T}: Add {C}.\n{T}: Add one mana of any color. Spend this mana only to cast a creature spell of the chosen type, and that spell can't be countered.";
const DISCOVERY: &str = "As this enchantment enters, choose a creature type.\nWhenever a creature you control of the chosen type enters or attacks, draw a card.";
const KILLER: &str = "When this enchantment enters, create a 1/1 white Human creature token, a 1/1 blue Merfolk creature token, and a 1/1 red Goblin creature token. Then secretly choose Human, Merfolk, or Goblin.\nSacrifice this enchantment, Reveal the creature type you chose: If target attacking creature token is the chosen type, put three +1/+1 counters on it and it gains deathtouch until end of turn.";

fn entry(name: &str, count: u32) -> DeckEntry {
    // Printed type lines checked against AtomicCards; abilities are irrelevant
    // to this display ranking (notably, Changeling is never expanded).
    let (core_types, subtypes) = match name {
        "Llanowar Elves" => (vec![CoreType::Creature], vec!["Elf", "Druid"]),
        "Elvish Mystic" => (vec![CoreType::Creature], vec!["Elf", "Druid"]),
        "Changeling Outcast" => (vec![CoreType::Creature], vec!["Shapeshifter"]),
        "Universal Automaton" => (
            vec![CoreType::Artifact, CoreType::Creature],
            vec!["Shapeshifter"],
        ),
        "Bitterblossom" => (
            vec![CoreType::Kindred, CoreType::Enchantment],
            vec!["Faerie"],
        ),
        "Grizzly Bears" => (vec![CoreType::Creature], vec!["Bear"]),
        "Goblin Guide" => (vec![CoreType::Creature], vec!["Goblin", "Scout"]),
        _ => panic!("missing printed fixture: {name}"),
    };
    DeckEntry {
        card: CardFace {
            name: name.into(),
            card_type: CardType {
                core_types,
                subtypes: subtypes.into_iter().map(str::to_string).collect(),
                ..Default::default()
            },
            ..Default::default()
        },
        count,
    }
}

fn install_pools(state: &mut GameState) {
    state.all_creature_types = [
        "Bear",
        "Dragon",
        "Druid",
        "Elf",
        "Faerie",
        "Goblin",
        "Human",
        "Merfolk",
        "Shapeshifter",
        "Warrior",
    ]
    .map(str::to_string)
    .to_vec();
    state.deck_pools = vec![
        PlayerDeckPool {
            player: P0,
            current_main: Arc::new(vec![
                entry("Llanowar Elves", 3),
                entry("Changeling Outcast", 1),
                entry("Universal Automaton", 1),
            ]),
            current_commander: Arc::new(vec![entry("Elvish Mystic", 1)]),
            current_companion: Arc::new(vec![entry("Grizzly Bears", 3)]),
            current_signature_spell: Arc::new(vec![entry("Bitterblossom", 1)]),
            current_sideboard: Arc::new(vec![entry("Goblin Guide", 4), entry("Grizzly Bears", 4)]),
            ..Default::default()
        },
        PlayerDeckPool {
            player: P1,
            current_main: Arc::new(vec![entry("Goblin Guide", 20)]),
            ..Default::default()
        },
    ];
    state.outside_game_cards_brought_in = vec![
        OutsideGameCardUse {
            player: P0,
            sideboard_index: 0,
            count: 1,
        },
        OutsideGameCardUse {
            player: P1,
            sideboard_index: 0,
            count: 20,
        },
    ];
}

fn expected() -> Vec<String> {
    [
        "Druid",
        "Elf",
        "Bear",
        "Shapeshifter",
        "Faerie",
        "Goblin",
        "Scout",
    ]
    .map(str::to_string)
    .to_vec()
}

fn suggestions(state: &GameState, viewer: Option<PlayerId>) -> Vec<String> {
    let filtered = match viewer {
        Some(viewer) => filter_state_for_viewer(state, viewer),
        None => filter_state_for_unseated_viewer(state),
    };
    derive_filtered_views(state, &filtered, viewer).named_choice_suggestions
}

fn cavern_prompt() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let cavern = scenario
        .add_land_to_hand(P0, "Cavern of Souls")
        .from_oracle_text(CAVERN)
        .id();
    // Owned creatures spread across zones, including one under opposing control.
    let owned = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    scenario.add_creature_to_hand(P0, "Llanowar Elves", 1, 1);
    scenario.add_creature_to_graveyard(P0, "Llanowar Elves", 1, 1);
    scenario.add_creature_to_exile(P0, "Changeling Outcast", 1, 1);
    scenario.add_card_to_library_top(P0, "Universal Automaton");
    let commander = scenario
        .add_creature_to_hand(P0, "Elvish Mystic", 1, 1)
        .id();
    scenario.with_commander(commander);
    let stolen = scenario
        .add_creature(P1, "Goblin Guide", 2, 2)
        .with_subtypes(vec!["Goblin", "Scout"])
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&owned)
        .unwrap()
        .controller = P1;
    runner
        .state_mut()
        .objects
        .get_mut(&stolen)
        .unwrap()
        .controller = P0;
    install_pools(runner.state_mut());
    runner.state_mut().all_creature_types.push("Scout".into());
    let card_id = runner.state().objects[&cavern].card_id;
    let result = runner
        .act(GameAction::PlayLand {
            object_id: cavern,
            card_id,
        })
        .unwrap();
    let raw = serde_json::to_value(result).unwrap();
    assert!(!raw.to_string().contains("named_choice_suggestions"));
    let WaitingFor::NamedChoice {
        player,
        choice_type: ChoiceType::CreatureType { options: explicit },
        options,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Cavern's as-enters prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(explicit.is_empty());
    assert_eq!(options.len(), runner.state().all_creature_types.len());
    assert!(options.contains(&"Dragon".into()));
    (runner, cavern)
}

#[test]
fn cavern_land_play_ranks_registered_printed_types_and_completes_choice() {
    let (mut runner, cavern) = cavern_prompt();
    assert_eq!(suggestions(runner.state(), Some(P0)), expected());
    runner
        .act(GameAction::ChooseOption {
            choice: "Elf".into(),
        })
        .unwrap();
    assert_eq!(runner.state().objects[&cavern].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&cavern].chosen_subtype_str(&ChosenSubtypeKind::CreatureType),
        Some("Elf".into())
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(suggestions(runner.state(), Some(P0)).is_empty());
}

#[test]
fn viewer_envelopes_disclose_suggestions_only_to_the_prompt_submitter() {
    let (mut runner, _) = cavern_prompt();
    assert_eq!(suggestions(runner.state(), Some(P0)), expected());
    for viewer in [Some(P0), Some(P1), None] {
        let wire = serde_json::to_value(ClientGameStateRef::wrap(runner.state(), viewer)).unwrap();
        if viewer == Some(P0) {
            assert_eq!(
                wire["derived"]["named_choice_suggestions"],
                serde_json::json!(expected())
            );
        } else {
            assert!(wire["derived"].get("named_choice_suggestions").is_none());
        }
        assert!(wire["state"]["waiting_for"]["data"]
            .get("named_choice_suggestions")
            .is_none());
    }
    assert!(derive_views(runner.state(), Some(P0))
        .named_choice_suggestions
        .is_empty());
    assert!(derive_views(runner.state(), None)
        .named_choice_suggestions
        .is_empty());
    let legacy: DerivedViews = serde_json::from_str("{}").unwrap();
    assert!(legacy.named_choice_suggestions.is_empty());
    // CR 723.4: under turn control the controller submits for P0, but the
    // registered pool is information about cards outside the game, so
    // neither seat receives pool-derived suggestions.
    runner.state_mut().turn_decision_controller = Some(P1);
    assert!(suggestions(runner.state(), Some(P0)).is_empty());
    assert!(suggestions(runner.state(), Some(P1)).is_empty());
    // The controller still sees, and can submit, every legal creature type.
    let full_options = match &runner.state().waiting_for {
        WaitingFor::NamedChoice { options, .. } => options.clone(),
        other => panic!("expected the creature type choice, got {other:?}"),
    };
    match &filter_state_for_viewer(runner.state(), P1).waiting_for {
        WaitingFor::NamedChoice { options, .. } => assert_eq!(options, &full_options),
        other => panic!("expected the creature type choice, got {other:?}"),
    }
}

#[test]
fn live_tokens_copies_face_down_and_transformed_objects_do_not_change_ranking() {
    let (mut runner, cavern) = cavern_prompt();
    let baseline = suggestions(runner.state(), Some(P0));
    assert_eq!(baseline, expected());
    // Hostile live characteristics must never replace the registered faces.
    for (_, object) in runner
        .state_mut()
        .objects
        .iter_mut()
        .filter(|(id, _)| **id != cavern)
    {
        object.card_types.subtypes = vec!["Dragon".into()];
        object.face_down = true;
        object.transformed = true;
        object.is_token = true;
    }
    assert_eq!(suggestions(runner.state(), Some(P0)), baseline);
    // A runtime copy not present in the registered pool adds no printed type.
    let mut copy = runner.state().objects[&cavern].clone();
    copy.id = ObjectId(9000);
    copy.card_types = CardType {
        core_types: vec![CoreType::Creature],
        subtypes: vec!["Dragon".into()],
        ..Default::default()
    };
    runner.state_mut().objects.insert(copy.id, copy);
    assert_eq!(suggestions(runner.state(), Some(P0)), baseline);
}

#[test]
fn legal_intersection_per_copy_deduplication_and_missing_pool_fallback() {
    let (mut runner, _) = cavern_prompt();
    let pool = &mut runner.state_mut().deck_pools[0];
    Arc::make_mut(&mut pool.current_main)[0]
        .card
        .card_type
        .subtypes
        .push("Elf".into());
    assert_eq!(
        choose::suggested_creature_types(runner.state(), P0, &["Elf".into(), "Dragon".into()]),
        vec!["Elf"]
    );
    assert_eq!(suggestions(runner.state(), Some(P0)), expected());
    runner.state_mut().deck_pools.clear();
    assert!(suggestions(runner.state(), Some(P0)).is_empty());
    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!("prompt lost")
    };
    assert!(options.contains(&"Dragon".into()));
}

#[test]
fn kindred_discovery_cast_has_private_suggestions_and_stores_answer() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let discovery = scenario
        .add_creature_to_hand(P0, "Kindred Discovery", 0, 0)
        .as_enchantment()
        .from_oracle_text(DISCOVERY)
        .id();
    let mut runner = scenario.build();
    install_pools(runner.state_mut());
    runner.state_mut().all_creature_types.push("Scout".into());
    runner.cast(discovery).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    assert_eq!(suggestions(runner.state(), Some(P0)), expected());
    runner
        .act(GameAction::ChooseOption {
            choice: "Faerie".into(),
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&discovery].chosen_subtype_str(&ChosenSubtypeKind::CreatureType),
        Some("Faerie".into())
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

#[test]
fn explicit_killer_options_keep_oracle_order_and_no_suggestion() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let killer = scenario
        .add_creature_to_hand(P0, "A Killer Among Us", 0, 0)
        .as_enchantment()
        .from_oracle_text(KILLER)
        .id();
    let mut runner = scenario.build();
    install_pools(runner.state_mut());
    runner.cast(killer).resolve();
    let WaitingFor::NamedChoice {
        options,
        choice_type: ChoiceType::CreatureType { options: explicit },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected restricted prompt: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(options, &["Human", "Merfolk", "Goblin"]);
    assert_eq!(explicit, options);
    assert!(suggestions(runner.state(), Some(P0)).is_empty());
    runner
        .act(GameAction::ChooseOption {
            choice: "Goblin".into(),
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

fn typed_face(name: &str, core: CoreType, subtypes: &[&str], mv: u32) -> CardFace {
    CardFace {
        name: name.to_string(),
        mana_cost: engine::types::mana::ManaCost::Cost {
            shards: vec![],
            generic: mv,
        },
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![core],
            subtypes: subtypes.iter().map(|subtype| subtype.to_string()).collect(),
        },
        ..Default::default()
    }
}

/// CR 702.139a: a companion revealed through the production pregame
/// declaration leaves the registered pool for the player record; its creature
/// types must still be counted, exactly once. CR 100.4b + CR 400.11a: a
/// declined Limited companion becomes an ordinary sideboard card, which is not
/// counted unless brought into the game.
#[test]
fn pregame_companion_declaration_keeps_its_creature_types_counted_once() {
    use engine::game::deck_loading::{load_deck_into_state, DeckPayload, PlayerDeckPayload};
    use engine::game::{apply, start_game_with_starting_player};
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{
        CompanionChoiceSource, CompanionDeclaration, CompanionRevealChoice,
    };
    use engine::types::keywords::{CompanionCondition, Keyword};

    let lurrus = DeckEntry {
        card: CardFace {
            keywords: vec![Keyword::Companion(
                CompanionCondition::MaxPermanentManaValue(2),
            )],
            ..typed_face(
                "Lurrus of the Dream-Den",
                CoreType::Creature,
                &["Cat", "Nightmare"],
                3,
            )
        },
        count: 1,
    };
    let mut main_deck = vec![DeckEntry {
        card: typed_face("Plains", CoreType::Land, &["Plains"], 0),
        count: 17,
    }];
    main_deck.extend((0..22).map(|i| DeckEntry {
        card: typed_face(&format!("Villager {i}"), CoreType::Creature, &["Human"], 2),
        count: 1,
    }));
    // One Bat ties a once-counted Lurrus and sorts before Cat, so counting
    // the companion twice would put Cat ahead of Bat.
    main_deck.push(DeckEntry {
        card: typed_face("Night Flyer", CoreType::Creature, &["Bat"], 2),
        count: 1,
    });
    let legal: Vec<String> = ["Bat", "Bear", "Cat", "Human", "Nightmare"]
        .map(str::to_string)
        .to_vec();
    let reveal = CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Lurrus of the Dream-Den".to_string(),
        source: CompanionChoiceSource::Dedicated,
    });
    let with_lurrus = ["Human", "Bat", "Cat", "Nightmare"]
        .map(str::to_string)
        .to_vec();
    let without_lurrus = ["Human", "Bat"].map(str::to_string).to_vec();
    for (label, declaration, expected) in [
        ("revealed", reveal, with_lurrus),
        ("declined", CompanionDeclaration::Decline, without_lurrus),
    ] {
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: main_deck.clone(),
                companion: vec![lurrus.clone()],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: main_deck.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::limited(), 2, 42);
        load_deck_into_state(&mut state, &payload);
        let start = start_game_with_starting_player(&mut state, P0);
        state.waiting_for = start.waiting_for;
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::CompanionReveal { player: P0, .. }
            ),
            "{label}: the dedicated Lurrus is offered"
        );
        apply(
            &mut state,
            P0,
            GameAction::DeclareCompanion {
                choice: declaration,
            },
        )
        .unwrap_or_else(|e| panic!("{label}: companion declaration failed: {e}"));
        let revealed = state.players[0].companion.is_some();
        assert_eq!(revealed, label == "revealed", "{label}");
        assert_eq!(
            choose::suggested_creature_types(&state, P0, &legal),
            expected,
            "{label}"
        );
    }
}
