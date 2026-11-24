use engine::ai_support::legal_actions_full;
use engine::game::casting::can_cast_object_now;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PLANET_STATION: &str = "Station (Tap another creature you control: Put charge counters equal to its power on this Planet. Station only as a sorcery.)";
const UTHROS: &str = "Uthros, Titanic Godcore";
const ADAGIA: &str = "Adagia, Windswept Bastion";
const KAVARON: &str = "Kavaron, Memorial World";
const HEARTHHULL: &str = "Hearthhull, the Worldseed";
const SERIEMA: &str = "The Seriema";
const P2: PlayerId = PlayerId(2);

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

fn add_planet(
    scenario: &mut GameScenario,
    name: &str,
    mana: &str,
    threshold: &str,
    counters: u32,
) -> ObjectId {
    let oracle =
        format!("This land enters tapped.\n{{T}}: Add {mana}.\n{PLANET_STATION}\n{threshold}");
    let id = scenario
        .add_land(P0, name)
        .with_subtypes(vec!["Planet"])
        .from_oracle_text_with_keywords(&["Station"], &oracle)
        .id();
    scenario.with_counter(id, charge(), counters);
    id
}

fn fund(scenario: &mut GameScenario, source: ObjectId, colors: &[ManaType]) {
    scenario.with_mana_pool(
        P0,
        colors
            .iter()
            .map(|&color| ManaUnit::new(color, source, false, vec![]))
            .collect(),
    );
}

fn activate(source_id: ObjectId, ability_index: usize) -> GameAction {
    GameAction::ActivateAbility {
        source_id,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    }
}

fn assert_priority(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn land_mana_row(runner: &GameRunner, land: ObjectId, index: usize) -> Option<GameAction> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped.into_values().flatten().find(|action| {
        matches!(
            action,
            GameAction::TapLandForMana { selection }
                if selection.source.object_id == land && selection.ability_index == Some(index)
        )
    })
}

fn uthros_board(counters: u32) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = add_planet(
        &mut scenario,
        UTHROS,
        "{U}",
        "12+ | {U}, {T}: Add {U} for each artifact you control.",
        counters,
    );
    fund(&mut scenario, land, &[ManaType::Blue]);
    for _ in 0..3 {
        scenario.add_artifact_from_oracle(P0, "Darksteel Relic", "Indestructible");
    }
    let contributor = scenario
        .add_creature(P0, "Ghalta, Primal Hunger", 12, 12)
        .as_legendary()
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(parse_mtgjson_mana_cost("{10}{G}{G}"))
        .from_oracle_text("This spell costs {X} less to cast, where X is the total power of creatures you control.\nTrample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)")
        .id();
    let djinn = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Tempest Djinn",
            0,
            4,
            "Flying\nThis creature gets +1/+0 for each basic Island you control.",
        )
        .with_subtypes(vec!["Djinn"])
        .with_mana_cost(parse_mtgjson_mana_cost("{U}{U}{U}"))
        .id();
    (scenario.build(), land, contributor, djinn)
}

#[test]
fn uthros_gates_mana_actions_and_payment() {
    let (mut below, land, _, djinn) = uthros_board(11);
    let (mut at, at_land, _, at_djinn) = uthros_board(12);
    let threshold = land_mana_row(&at, at_land, 1).expect("12+ mana row");
    // CR 721.2a: the live action and the payment planner require the threshold.
    assert!(land_mana_row(&below, land, 1).is_none());
    assert!(below.act(threshold.clone()).is_err());
    assert!(!can_cast_object_now(below.state(), P0, djinn));
    // CR 721.4: the ordinary mana ability remains usable.
    let ordinary = land_mana_row(&below, land, 0).expect("ordinary mana row");
    below.act(ordinary).unwrap();
    assert_eq!(
        below.state().players[0]
            .mana_pool
            .count_color(ManaType::Blue),
        2
    );

    at.act(threshold).unwrap();
    assert_priority(&at);
    assert_eq!(
        at.state().players[0].mana_pool.count_color(ManaType::Blue),
        3
    );
    let outcome = at.cast(at_djinn).resolve();
    outcome.assert_zone(&[at_djinn], Zone::Battlefield);
    assert_priority(&at);
}

#[test]
fn uthros_pays_for_djinn_after_stationing_and_explicit_activation() {
    let (mut runner, land, contributor, djinn) = uthros_board(0);
    // CR 721.2a: stationing unlocks a mana ability that was absent at zero.
    assert!(land_mana_row(&runner, land, 1).is_none());
    runner
        .act(GameAction::ActivateStation {
            spacecraft_id: land,
            creature_id: None,
        })
        .unwrap();
    let WaitingFor::StationTarget {
        player,
        spacecraft_id,
        eligible_creatures,
    } = &runner.state().waiting_for
    else {
        panic!("expected station selection");
    };
    assert_eq!((*player, *spacecraft_id), (P0, land));
    assert!(eligible_creatures.contains(&contributor));
    runner
        .act(GameAction::ActivateStation {
            spacecraft_id: land,
            creature_id: Some(contributor),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner);
    assert_eq!(runner.state().objects[&land].counters[&charge()], 12);
    assert!(runner.state().objects[&contributor].tapped);
    // The automatic planner requires independent funding for a counted mana
    // ability's mana cost. Explicitly spend the floating blue on Uthros.
    let threshold = land_mana_row(&runner, land, 1).expect("station unlocked the mana row");
    runner.act(threshold).unwrap();
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Blue),
        3
    );
    let outcome = runner.cast(djinn).resolve();
    outcome.assert_zone(&[djinn], Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

/// Uthros with an empty pool and one untapped Island to pay its `{U}`.
fn uthros_island_board(counters: u32) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = add_planet(
        &mut scenario,
        UTHROS,
        "{U}",
        "12+ | {U}, {T}: Add {U} for each artifact you control.",
        counters,
    );
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    for _ in 0..3 {
        scenario.add_artifact_from_oracle(P0, "Darksteel Relic", "Indestructible");
    }
    let djinn = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Tempest Djinn",
            0,
            4,
            "Flying\nThis creature gets +1/+0 for each basic Island you control.",
        )
        .with_subtypes(vec!["Djinn"])
        .with_mana_cost(parse_mtgjson_mana_cost("{U}{U}{U}"))
        .id();
    (scenario.build(), land, island, djinn)
}

#[test]
fn uthros_threshold_ability_funds_an_automatic_payment() {
    // CR 721.2a: at 11 counters the Island and Uthros's printed ability make
    // only two blue, so the cast cannot be funded.
    let (below, _, _, djinn) = uthros_island_board(11);
    assert!(!can_cast_object_now(below.state(), P0, djinn));

    // CR 605.3b + CR 721.2a: at 12 the payment taps the Island for the
    // threshold ability's {U}, which makes three blue for the Djinn.
    let (mut at, land, island, djinn) = uthros_island_board(12);
    assert!(can_cast_object_now(at.state(), P0, djinn));
    let outcome = at.cast(djinn).resolve();
    outcome.assert_zone(&[djinn], Zone::Battlefield);
    let state = outcome.state();
    assert!(state.objects[&land].tapped);
    assert!(state.objects[&island].tapped);
    assert!(state.stack.is_empty());
}

fn adagia_board(phase: Phase, counters: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let land = add_planet(
        &mut scenario, ADAGIA, "{W}",
        "12+ | {3}{W}, {T}: Create a token that's a copy of target artifact or enchantment you control, except it's legendary. Activate only as a sorcery.",
        counters,
    );
    fund(&mut scenario, land, &[ManaType::White; 4]);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Darksteel Relic", "Indestructible")
        .id();
    (scenario.build(), land, artifact)
}

#[test]
fn adagia_requires_threshold_and_sorcery_timing_for_legendary_copy() {
    // CR 721.2a: the copy ability is unavailable at 11 charge counters.
    let (mut below, land, _) = adagia_board(Phase::PreCombatMain, 11);
    assert!(below.act(activate(land, 1)).is_err());
    // CR 307.5: the printed timing restriction still applies at 12.
    let (mut combat, land, _) = adagia_board(Phase::BeginCombat, 12);
    assert!(combat.act(activate(land, 1)).is_err());

    let (mut at, land, original) = adagia_board(Phase::PreCombatMain, 12);
    let outcome = at.activate(land, 1).target_object(original).resolve();
    outcome.assert_zone(&[original], Zone::Battlefield);
    let state = outcome.state();
    let tokens: Vec<_> = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|obj| obj.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].name, state.objects[&original].name);
    assert!(tokens[0]
        .card_types
        .supertypes
        .contains(&Supertype::Legendary));
    assert!(has_keyword(tokens[0], &Keyword::Indestructible));
    assert!(state.objects[&land].tapped);
    assert_priority(&at);
}

fn kavaron_board(counters: u32) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = add_planet(
        &mut scenario, KAVARON, "{R}",
        "12+ | {1}{R}, {T}, Sacrifice a land: Create a 2/2 colorless Robot artifact creature token, then creatures you control get +1/+0 and gain haste until end of turn.",
        counters,
    );
    fund(&mut scenario, land, &[ManaType::Red; 2]);
    let fodder = scenario.add_basic_land(P0, ManaColor::Red);
    let creature = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    (scenario.build(), land, fodder, creature)
}

#[test]
fn kavaron_requires_threshold_and_buffs_the_robot_it_creates() {
    // CR 721.2a: the sacrifice ability is unavailable at 11 charge counters.
    let (mut below, land, _, _) = kavaron_board(11);
    assert!(below.act(activate(land, 1)).is_err());

    let (mut at, land, fodder, creature) = kavaron_board(12);
    let outcome = at.activate(land, 1).pay_with(&[fodder]).resolve();
    outcome.assert_zone(&[fodder], Zone::Graveyard);
    let state = outcome.state();
    let tokens: Vec<_> = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|obj| obj.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    let robot = tokens[0];
    assert!(robot.color.is_empty());
    assert!(robot.card_types.core_types.contains(&CoreType::Artifact));
    assert!(robot.card_types.core_types.contains(&CoreType::Creature));
    assert!(robot.card_types.subtypes.iter().any(|name| name == "Robot"));
    assert_eq!((robot.base_power, robot.base_toughness), (Some(2), Some(2)));
    for obj in [robot, &state.objects[&creature]] {
        assert_eq!((obj.power, obj.toughness), (Some(3), Some(2)));
        assert!(has_keyword(obj, &Keyword::Haste));
    }
    assert!(state.objects[&land].tapped);
    assert_priority(&at);
}

fn hearthhull_board(counters: u32) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ship = scenario.add_creature(P0, HEARTHHULL, 6, 7)
        .as_artifact()
        .as_legendary()
        .with_subtypes(vec!["Spacecraft"])
        .from_oracle_text_with_keywords(&["Station"], "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 8+.)\n2+ | {1}, {T}, Sacrifice a land: Draw two cards. You may play an additional land this turn.\n8+ | Flying, vigilance, haste\nWhenever you sacrifice a land, each opponent loses 2 life.")
        .id();
    let hexmage = scenario
        .add_creature(P0, "Vampire Hexmage", 2, 1)
        .with_subtypes(vec!["Vampire", "Shaman"])
        .with_mana_cost(parse_mtgjson_mana_cost("{B}{B}"))
        .from_oracle_text(
            "First strike\nSacrifice this creature: Remove all counters from target permanent.",
        )
        .id();
    let fodder = scenario.add_basic_land(P0, ManaColor::Blue);
    fund(&mut scenario, fodder, &[ManaType::Blue]);
    scenario.with_library_top(P0, &["Island", "Island"]);
    scenario.with_counter(ship, charge(), counters);
    (scenario.build(), ship, fodder, hexmage)
}

fn sacrifice_land(runner: &mut GameRunner, ship: ObjectId, fodder: ObjectId) {
    runner.act(activate(ship, 0)).unwrap();
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Sacrifice,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected land sacrifice cost");
    };
    assert_eq!((*player, *count), (P0, 1));
    assert!(choices.contains(&fodder));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
}

#[test]
fn hearthhull_drains_each_opponent_only_at_threshold() {
    for (counters, life) in [(7, 20), (8, 18)] {
        let (mut runner, ship, fodder, _) = hearthhull_board(counters);
        sacrifice_land(&mut runner, ship, fodder);
        // CR 721.2a: the event publishes the trigger only at 8+.
        assert_eq!(
            runner.state().stack.len(),
            if counters == 8 { 2 } else { 1 }
        );
        runner.advance_until_stack_empty();
        assert_priority(&runner);
        assert_eq!(runner.life(P0), 20);
        assert_eq!(runner.life(P1), life);
        assert_eq!(runner.life(P2), life);
    }
}

#[test]
fn hearthhull_drain_survives_counter_removal_in_response() {
    let (mut runner, ship, fodder, hexmage) = hearthhull_board(8);
    sacrifice_land(&mut runner, ship, fodder);
    assert!(matches!(
        runner.state().stack.back().map(|entry| &entry.kind),
        Some(StackEntryKind::TriggeredAbility { source_id, .. }) if *source_id == ship
    ));
    let outcome = runner.activate(hexmage, 0).target_object(ship).resolve();
    outcome.assert_zone(&[hexmage], Zone::Graveyard);
    assert_eq!(outcome.counters(ship, charge()), 0);
    // CR 113.7a: the queued trigger resolves even after the source loses the ability.
    assert_eq!(runner.life(P1), 18);
    assert_eq!(runner.life(P2), 18);
    assert_priority(&runner);
}

#[test]
fn seriema_continuation_grants_indestructible_only_to_other_tapped_legendary_creatures() {
    for counters in [6, 7] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ship = scenario.add_creature(P0, SERIEMA, 5, 5)
            .as_artifact()
            .as_legendary()
            .with_subtypes(vec!["Spacecraft"])
            .from_oracle_text_with_keywords(&["Station"], "When The Seriema enters, search your library for a legendary creature card, reveal it, put it into your hand, then shuffle.\nStation (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 7+.)\n7+ | Flying\nOther tapped legendary creatures you control have indestructible.")
            .id();
        scenario.with_counter(ship, charge(), counters);
        let tapped = scenario
            .add_creature(P0, "Isamaru, Hound of Konda", 2, 2)
            .as_legendary()
            .with_subtypes(vec!["Dog"])
            .id();
        let untapped = scenario.add_creature(P0, "Yoshimaru, Ever Faithful", 1, 1)
            .as_legendary().with_subtypes(vec!["Dog"])
            .from_oracle_text_with_keywords(&["Partner"], "Whenever another legendary permanent you control enters, put a +1/+1 counter on Yoshimaru.\nPartner (You can have two commanders if both have partner.)")
            .id();
        let ordinary = scenario
            .add_creature(P0, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .id();
        let opponent = scenario
            .add_creature(P1, "Isamaru, Hound of Konda", 2, 2)
            .as_legendary()
            .with_subtypes(vec!["Dog"])
            .id();
        let mut runner = scenario.build();
        for id in [ship, tapped, ordinary, opponent] {
            runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
        }
        runner.state_mut().layers_dirty.mark_full();
        evaluate_layers(runner.state_mut());
        let indestructible =
            |id| has_keyword(&runner.state().objects[&id], &Keyword::Indestructible);
        // CR 721.2a: the continuation's grant belongs to the 7+ striation.
        assert_eq!(indestructible(tapped), counters == 7);
        for id in [ship, untapped, ordinary, opponent] {
            assert!(!indestructible(id));
        }
    }
}
