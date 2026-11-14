//! Runtime witnesses for card-facing parse changes this PR makes: New New
//! York's Vehicle animation, the exact-two "other target" untap/tap cards, and
//! Mojave Desert's per-player shuffle.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
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
/// ability declares exactly two other legendary creatures. Both untap and a third
/// legendary creature that was not chosen stays tapped.
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

// Oracle texts from the repo's card data export.
const FIELD_OF_RUIN: &str = "{T}: Add {C}.\n{2}, {T}, Sacrifice this land: Destroy target nonbasic land an opponent controls. Each player searches their library for a basic land card, puts it onto the battlefield, then shuffles.";
const DAKKON_SHADOW_SLAYER: &str = "Dakkon enters with a number of loyalty counters on him equal to the number of lands you control.\n[+1]: Surveil 2.\n[\u{2212}3]: Exile target creature.\n[\u{2212}6]: You may put an artifact card from your hand or graveyard onto the battlefield.";

/// CR 701.24a + CR 608.2c: Field of Ruin's "Each player searches their library
/// for a basic land card, puts it onto the battlefield, then shuffles" shuffles
/// every player's library, once each. This is a base pin: the printed behaviour
/// is the same at the merge base, where the serialized scope was absent.
#[test]
fn field_of_ruin_shuffles_every_players_library_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let field = scenario
        .add_land_from_oracle(P0, "Field of Ruin", FIELD_OF_RUIN)
        .id();
    let victim = scenario
        .add_land_from_oracle(P1, "Victim Land", "{T}: Add {C}.")
        .id();
    let forest = scenario.add_card_to_library_top(P0, "Forest");
    let island = scenario.add_card_to_library_top(P1, "Island");
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    for id in [forest, island] {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Land);
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }

    let outcome = runner
        .activate(field, 1)
        .target_object(victim)
        .search_first_legal()
        .resolve();

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
        "each player shuffles their own library once"
    );
}

/// CR 608.2d + CR 115.1: Dakkon's "[\u{2212}6]: You may put an artifact card
/// from your hand or graveyard onto the battlefield" names no target, so the
/// choice is announced while the ability resolves, not when it is activated:
/// activating completes without any prompt, resolving offers both cards, and
/// the chosen one enters.
#[test]
fn dakkon_ultimate_chooses_its_artifact_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dakkon = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Dakkon, Shadow Slayer",
            "Dakkon",
            6,
            DAKKON_SHADOW_SLAYER,
        )
        .id();
    let in_hand = scenario
        .add_artifact_from_oracle(P0, "Hand Artifact", "{T}: Add {C}.")
        .id();
    let in_graveyard = scenario
        .add_artifact_from_oracle(P0, "Graveyard Artifact", "{T}: Add {C}.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        for (id, zone) in [
            (in_hand, engine::types::zones::Zone::Hand),
            (in_graveyard, engine::types::zones::Zone::Graveyard),
        ] {
            state.battlefield.retain(|&b| b != id);
            let mut events = vec![];
            engine::game::zones::move_to_zone(state, id, zone, &mut events);
        }
    }
    let ultimate = runner.state().objects[&dakkon].abilities.len() - 1;

    let activated = runner
        .activate(dakkon, ultimate)
        .stop_at_optional()
        .resolve();

    assert!(
        matches!(
            activated.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the printed \"You may\" is asked first, with no target or zone prompt at activation"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the player accepts the optional put");
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            let mut offered = cards.clone();
            offered.sort_by_key(|id| id.0);
            let mut expected = vec![in_hand, in_graveyard];
            expected.sort_by_key(|id| id.0);
            assert_eq!(offered, expected, "both zones are offered at resolution");
        }
        other => panic!("expected the resolution-time artifact choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![in_graveyard],
        })
        .expect("the graveyard artifact is chosen");
    assert_eq!(
        runner.state().objects[&in_graveyard].zone,
        engine::types::zones::Zone::Battlefield
    );
    assert_eq!(
        runner.state().objects[&in_hand].zone,
        engine::types::zones::Zone::Hand
    );
}

const GENESIS_ULTIMATUM: &str = "Look at the top five cards of your library. Put any number of permanent cards from among them onto the battlefield and the rest into your hand. Exile Genesis Ultimatum.";

/// CR 608.2c + CR 400.7: "Exile Genesis Ultimatum." names the spell itself, not
/// a card the dig put onto the battlefield: the spell must end in exile and the
/// dug permanent must stay on the battlefield. This is a base pin: it passes at
/// the merge base too (the serialized `forward_result` differs, the outcome does
/// not).
#[test]
fn genesis_ultimatum_exiles_itself_and_leaves_the_dug_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dug: Vec<_> = (0..5)
        .map(|i| scenario.add_card_to_library_top(P0, &format!("Dug Card {i}")))
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Genesis Ultimatum", false, GENESIS_ULTIMATUM)
        .id();
    let mut runner = scenario.build();
    for id in &dug {
        let object = runner.state_mut().objects.get_mut(id).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.base_card_types = object.card_types.clone();
    }

    runner.cast(spell).resolve();
    let mut guard = 0;
    while !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        let cards: Vec<ObjectId> = match &runner.state().waiting_for {
            WaitingFor::DigChoice { cards, .. } => vec![cards[0]],
            WaitingFor::EffectZoneChoice { cards, .. } => vec![cards[0]],
            other => panic!("unexpected prompt {other:?}"),
        };
        runner
            .act(GameAction::SelectCards { cards })
            .expect("a permanent is chosen");
        guard += 1;
        assert!(guard < 6);
    }

    let state = runner.state();
    assert_eq!(
        state.objects[&spell].zone,
        engine::types::zones::Zone::Exile,
        "the spell exiles itself"
    );
    assert!(
        dug.iter()
            .any(|id| state.objects[id].zone == engine::types::zones::Zone::Battlefield),
        "the chosen permanent stays on the battlefield"
    );
    assert!(
        dug.iter()
            .all(|id| state.objects[id].zone != engine::types::zones::Zone::Exile),
        "no dug card is exiled in its place"
    );
}

// Oracle text from the repo's card data export.
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";

/// CR 730.3c + CR 400.7e: "Exile target creature you control, then return that
/// card to the battlefield" finds the new objects a merged permanent became when
/// it left the battlefield, so the same return is taken on each of them: both
/// cards of the merged permanent come back, as separate permanents. Passes at the
/// merge base; it fails without the same-chain departure evidence in
/// `merge::referent_records_departure`.
#[test]
fn cloudshift_returns_every_card_of_a_merged_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Creature", 2, 2).id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        engine::game::merge::merge_object_onto(
            state,
            component,
            host,
            engine::game::merge::MergeSide::Top,
            &mut events,
        );
    }

    let outcome = runner.cast(spell).target_object(host).resolve();

    outcome.assert_zone(&[host, component], engine::types::zones::Zone::Battlefield);
}

// Oracle text from the repo's card data export.
const THAWING_GLACIERS: &str = "This land enters tapped.\n{1}, {T}: Search your library for a basic land card, put that card onto the battlefield tapped, then shuffle. Return this land to its owner's hand at the beginning of the next cleanup step.";

/// CR 608.2c + CR 400.7: "Return this land to its owner's hand at the beginning
/// of the next cleanup step" names Thawing Glaciers itself, not the basic land
/// the search just put onto the battlefield. This is a base pin: it passes at the
/// merge base too (its serialized `forward_result` differs, the outcome does not).
#[test]
fn thawing_glaciers_returns_itself_and_keeps_the_fetched_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let glaciers = scenario
        .add_land_from_oracle(P0, "Thawing Glaciers", THAWING_GLACIERS)
        .id();
    let basic = scenario.add_card_to_library_top(P0, "Forest");
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
    {
        let object = runner.state_mut().objects.get_mut(&basic).unwrap();
        object.card_types.core_types.push(CoreType::Land);
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }

    runner.activate(glaciers, 0).search_first_legal().resolve();
    assert_eq!(
        runner.state().objects[&basic].zone,
        engine::types::zones::Zone::Battlefield,
        "reach guard: the search put the basic land onto the battlefield"
    );

    let mut passes = 0;
    while runner.state().objects[&glaciers].zone == engine::types::zones::Zone::Battlefield {
        if let WaitingFor::DiscardToHandSize { .. } = &runner.state().waiting_for {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes toward the cleanup step");
        passes += 1;
        assert!(passes < 40, "the delayed return must happen at cleanup");
    }

    assert_eq!(
        runner.state().objects[&glaciers].zone,
        engine::types::zones::Zone::Hand,
        "Thawing Glaciers returns itself"
    );
    assert_eq!(
        runner.state().objects[&basic].zone,
        engine::types::zones::Zone::Battlefield,
        "the fetched land stays on the battlefield"
    );
}

// Oracle text from the repo's card data export.
const REUNION_OF_THE_HOUSE: &str = "Return any number of target creature cards with total power 10 or less from your graveyard to the battlefield. Exile Reunion of the House.";

/// CR 608.2c + CR 400.7: "Exile Reunion of the House." names the spell itself,
/// not a creature card the first instruction returned. On `main` the instruction
/// was rebound to the first returned creature (the creature went to exile and the
/// spell stayed in the graveyard); the spell must end in exile and the returned
/// creature must stay on the battlefield. Fails at the merge base.
#[test]
fn reunion_of_the_house_exiles_itself_and_leaves_the_returned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_graveyard(P0, "Fallen Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reunion of the House", false, REUNION_OF_THE_HOUSE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();

    outcome.assert_zone(&[creature], engine::types::zones::Zone::Battlefield);
    outcome.assert_zone(&[spell], engine::types::zones::Zone::Exile);
}
