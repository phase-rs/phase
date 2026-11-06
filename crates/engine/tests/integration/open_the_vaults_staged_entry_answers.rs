use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text from data/mtgjson/AtomicCards.json.
const VAULTS: &str = "Return all artifact and enchantment cards from all graveyards to the battlefield under their owners' control. (Auras with nothing to enchant remain in graveyards.)";
const MOX: &str = "If this artifact would enter, you may discard a land card instead. If you do, put this artifact onto the battlefield. If you don't, put it into its owner's graveyard.\n{T}: Add one mana of any color.";
const ROOT_MAZE: &str = "Artifacts and lands enter tapped.";
const ARCHELOS: &str = "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.";
const TWILIGHTS_CALL: &str = "You may cast this spell as though it had flash if you pay {2} more to cast it. (You may cast it any time you could cast an instant.)\nEach player returns all creature cards from their graveyard to the battlefield.";
const XANTCHA: &str = "Xantcha enters under the control of an opponent of your choice.\nXantcha attacks each combat if able and can't attack its owner or planeswalkers its owner controls.\n{3}: Xantcha's controller loses 2 life and you draw a card. Any player may activate this ability.";

struct VaultsBoard {
    scenario: GameScenario,
    mox: ObjectId,
    thopter: ObjectId,
    forest: ObjectId,
    island: ObjectId,
    vaults: ObjectId,
}

fn vaults_board() -> VaultsBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox = scenario
        .add_creature_to_graveyard(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX)
        .id();
    let thopter = scenario
        .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .from_oracle_text("Flying")
        .id();
    let forest = scenario
        .add_land_to_hand(P0, "Forest")
        .from_oracle_text("({T}: Add {G}.)")
        .id();
    let island = scenario
        .add_land_to_hand(P0, "Island")
        .from_oracle_text("({T}: Add {U}.)")
        .id();
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, VAULTS)
        .id();
    VaultsBoard {
        scenario,
        mox,
        thopter,
        forest,
        island,
        vaults,
    }
}

fn assert_settled(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().resolution_stack.is_empty(),
        "{:?}",
        runner.state().resolution_stack
    );
}

fn choose_source(runner: &mut GameRunner, source: ObjectId, expected_count: usize) {
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected replacement ordering, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates.len(), expected_count);
    let index = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("live ordering owner")
        .candidates
        .iter()
        .position(|rid| rid.source == source)
        .expect("offered replacement source");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choose offered ordering");
}

/// CR 614.12a + CR 616.1: answering an entry cost finishes its whole staged population.
#[test]
fn open_the_vaults_returns_mox_diamond_after_discard_choice_and_the_other_artifact() {
    let VaultsBoard {
        scenario,
        mox,
        thopter,
        forest,
        island,
        vaults,
    } = vaults_board();
    let mut runner = scenario.build();
    runner.cast(vaults).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Mox discard");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("pay Mox discard");
    assert_eq!(runner.state().objects[&mox].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&thopter].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&island].zone, Zone::Hand);
    assert_settled(&runner);
}

/// CR 614.12a + CR 616.1: a paid cost can expose another noncommuting ordering choice.
#[test]
fn open_the_vaults_mox_diamond_orders_root_maze_against_archelos() {
    let VaultsBoard {
        mut scenario,
        mox,
        thopter,
        forest,
        island,
        vaults,
    } = vaults_board();
    let root_maze = scenario
        .add_enchantment_from_oracle(P0, "Root Maze", ROOT_MAZE)
        .id();
    let archelos = scenario
        .add_creature_from_oracle(P1, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS)
        .id();
    let mut runner = scenario.build();
    assert!(!runner.state().objects[&archelos].tapped);
    runner.cast(vaults).resolve();
    choose_source(&mut runner, mox, 3);
    assert!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .is_optional
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Mox discard");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("pay Mox discard");
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("Mox ordering survives discard");
    assert_eq!(pending.proposed.affected_object_id(), Some(mox));
    assert!(pending.candidates.iter().any(|rid| rid.source == archelos));
    choose_source(&mut runner, root_maze, 2);
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("Ornithopter has its own ordering");
    assert_eq!(pending.proposed.affected_object_id(), Some(thopter));
    assert!(pending.candidates.iter().any(|rid| rid.source == archelos));
    choose_source(&mut runner, root_maze, 2);
    for id in [mox, thopter] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        assert!(
            !runner.state().objects[&id].tapped,
            "Archelos's later write wins"
        );
    }
    assert_eq!(runner.state().objects[&forest].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&island].zone, Zone::Hand);
    assert_settled(&runner);
}

/// CR 614.12a + CR 616.1b: the chosen entry controller is bound before delivery.
#[test]
fn twilights_call_xantcha_entry_controller_choice_settles_both_entrants() {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 0x326);
    scenario.at_phase(Phase::PreCombatMain);
    let xantcha = scenario
        .add_creature_to_graveyard(P0, "Xantcha, Sleeper Agent", 5, 5)
        .from_oracle_text(XANTCHA)
        .id();
    let thopter = scenario
        .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .from_oracle_text("Flying")
        .id();
    let call = scenario
        .add_spell_to_hand_from_oracle(P0, "Twilight's Call", false, TWILIGHTS_CALL)
        .id();
    let mut runner = scenario.build();
    runner.cast(call).resolve();
    let WaitingFor::EntryControllerChoice { player, candidates } = &runner.state().waiting_for
    else {
        panic!(
            "expected entry controller, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates, &[P1, P2]);
    assert_eq!(runner.state().objects[&xantcha].zone, Zone::Graveyard);
    runner
        .act(GameAction::ChooseEntryController { opponent: P2 })
        .expect("choose Xantcha's controller");
    assert_eq!(runner.state().objects[&xantcha].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&xantcha].controller, P2);
    assert_eq!(runner.state().objects[&thopter].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&thopter].controller, P0);
    assert_settled(&runner);
}
