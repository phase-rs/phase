//! Runtime witnesses for card-facing parse changes this PR makes: New New
//! York's Vehicle animation, the exact-two "other target" untap/tap cards, and
//! Mojave Desert's per-player shuffle.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

// Oracle text from the repo's card data export (the combat trigger line only).
const NEW_NEW_YORK_COMBAT_TRIGGER: &str = "At the beginning of combat on your turn, until end of turn, noncreature artifacts you control become 3/3 Vehicles in addition to their other types and gain flying, haste, and crew 1.";
const TREASURE: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";

fn is_creature(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

/// CR 205.3 + CR 301.7 + CR 702.122a: New New York's noncreature artifacts
/// become 3/3 Vehicles with crew 1 at the beginning of combat. A Vehicle is a
/// creature only while crewed, so the Treasure is not a creature until its crew
/// cost is paid.
///
/// SYNTHETIC host: New New York is a Plane card, which is not a permanent. The
/// printed combat-trigger line is put on an enchantment so the real trigger,
/// animation and crew pipeline run; New New York is the nearest printed card.
#[test]
fn new_new_york_makes_noncreature_artifacts_vehicles_that_become_creatures_only_when_crewed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Synthetic New New York", NEW_NEW_YORK_COMBAT_TRIGGER);
    let treasure = scenario
        .add_artifact_from_oracle(P0, "Treasure", TREASURE)
        .id();
    let pilot = scenario.add_creature(P0, "Pilot", 2, 2).id();
    let mut runner = scenario.build();
    assert!(
        !is_creature(&runner, treasure),
        "reach guard: a plain artifact"
    );

    let mut passes = 0;
    while !(runner.state().phase == Phase::BeginCombat && !runner.state().stack.is_empty()) {
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes toward the beginning of combat");
        passes += 1;
        assert!(passes < 12, "the combat trigger must go on the stack");
    }
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("the combat trigger resolves");
    }

    let object = &runner.state().objects[&treasure];
    assert!(
        object.card_types.subtypes.iter().any(|s| s == "Vehicle"),
        "the Treasure became a Vehicle"
    );
    assert_eq!((object.power, object.toughness), (Some(3), Some(3)));
    assert!(object.has_keyword(&Keyword::Flying));
    assert!(object.has_keyword(&Keyword::Haste));
    assert!(
        !is_creature(&runner, treasure),
        "a Vehicle is not a creature until crewed (CR 301.7)"
    );

    // CR 702.122a: pay crew 1 through the public selection prompt.
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: treasure,
            creature_ids: vec![],
        })
        .expect("the Vehicle can be crewed");
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::CrewVehicle { vehicle_id, eligible_creatures, .. }
            if *vehicle_id == treasure && eligible_creatures.contains(&pilot)
    ));
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: treasure,
            creature_ids: vec![pilot],
        })
        .expect("the crew cost is paid");
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&pilot].tapped, "crew taps the pilot");
    assert!(
        is_creature(&runner, treasure),
        "crewing makes the Vehicle an artifact creature"
    );
}

// Oracle text from the repo's card data export.
const IORETH: &str = "{T}: Untap another target permanent.
{T}: Untap two other target legendary creatures.";

/// CR 115.1c + CR 115.1d + CR 602.2: Ioreth of the Healing House's second
/// ability declares exactly two other legendary creatures. Both untap, a third
/// legendary creature stays tapped, and Ioreth itself is not a legal target.
#[test]
fn ioreth_untaps_exactly_the_two_other_legendary_creatures_it_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ioreth = {
        let mut builder = scenario.add_creature(P0, "Ioreth of the Healing House", 3, 3);
        builder
            .as_legendary()
            .from_oracle_text_with_keywords(&[], IORETH);
        builder.id()
    };
    let mut legendary = |name: &str| {
        let mut builder = scenario.add_creature(P0, name, 2, 2);
        builder.as_legendary();
        builder.id()
    };
    let first = legendary("Legend One");
    let second = legendary("Legend Two");
    let bystander = legendary("Legend Three");
    let mut runner = scenario.build();
    for id in [first, second, bystander] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    assert!(!runner.state().objects[&ioreth].tapped);

    let outcome = runner
        .activate(ioreth, 1)
        .target_objects(&[first, second])
        .resolve();

    let state = outcome.state();
    assert!(state.objects[&ioreth].tapped, "the {{T}} cost is paid");
    assert!(!state.objects[&first].tapped);
    assert!(!state.objects[&second].tapped);
    assert!(
        state.objects[&bystander].tapped,
        "only the two chosen creatures untap"
    );
}

// The printed clause of Mojave Desert's planeswalk trigger, as a sorcery.
const EACH_PLAYER_SEARCH_AND_SHUFFLE: &str = "Each player searches their library for up to two basic land or Desert cards and puts them onto the battlefield tapped, then shuffles their library.";

/// CR 701.24a + CR 608.2c: "each player searches ... then shuffles their
/// library" shuffles each player's own library, once.
///
/// SYNTHETIC host: Mojave Desert's trigger fires on planeswalking, which the
/// scenario runner does not model, so the printed clause is cast as a sorcery
/// (Mojave Desert itself stays unsupported for its other clauses).
#[test]
fn each_player_search_then_shuffle_shuffles_every_players_own_library_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Library A", "P0 Library B"]);
    scenario.with_library_top(P1, &["P1 Library A", "P1 Library B"]);
    // A basic land in each library, so each search finds a card.
    let lands: Vec<_> = [P0, P1]
        .into_iter()
        .map(|player| {
            let land = scenario.add_land_to_library_top(player, "Plains").id();
            land
        })
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Mojave Clause",
            false,
            EACH_PLAYER_SEARCH_AND_SHUFFLE,
        )
        .id();
    let mut runner = scenario.build();
    for land in &lands {
        runner
            .state_mut()
            .objects
            .get_mut(land)
            .unwrap()
            .card_types
            .supertypes
            .push(engine::types::card_type::Supertype::Basic);
    }

    let outcome = runner.cast(spell).search_first_legal().resolve();

    let shuffled: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shuffled,
        vec![P0, P1],
        "each player shuffles their own library, once, in turn order"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// Fail-to-find control: with no matching card in either library the searches
/// find nothing, and "then shuffles their library" still shuffles each
/// player's library (CR 701.24a; a search that finds nothing is still
/// followed by its shuffle instruction).
#[test]
fn each_player_search_that_finds_nothing_still_shuffles_every_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Library A", "P0 Library B"]);
    scenario.with_library_top(P1, &["P1 Library A", "P1 Library B"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Mojave Clause",
            false,
            EACH_PLAYER_SEARCH_AND_SHUFFLE,
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).search_none().resolve();

    let shuffled: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(shuffled, vec![P0, P1]);
}
