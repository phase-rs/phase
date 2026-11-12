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

/// CR 101.4c + CR 101.4d + CR 616.1: the staged population's sweep consults
/// every entrant before any prompt, and the order restarts after every answer
/// (an answer that re-parks the same member holds that decision again). While
/// P0 holds two or more decisions, it first chooses which to make; make
/// `subject`'s first. `held` is every member that holds a decision, in
/// announcement order. Returns whether P0 was asked to order.
fn make_decision_first(runner: &mut GameRunner, held: &[ObjectId], subject: ObjectId) -> bool {
    let WaitingFor::ReplacementDecisionOrder { player, objects } =
        runner.state().waiting_for.clone()
    else {
        return false;
    };
    assert_eq!(player, P0);
    assert_eq!(objects, held, "P0 orders exactly its held decisions");
    runner
        .act(GameAction::SelectCards {
            cards: vec![subject],
        })
        .expect("make the chosen held decision first");
    true
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
    // Both entrants hold an ordering decision after the sweep, so P0 first
    // chooses which to make; the Mox's goes first, as before.
    assert!(
        make_decision_first(&mut runner, &[mox, thopter], mox),
        "P0 holds both entrants' orderings after the sweep"
    );
    choose_source(&mut runner, mox, 3);
    // CR 101.4d: the answer re-parks the Mox on its own optional discard, so
    // the order restarts and P0 again chooses which held decision to make.
    assert!(
        make_decision_first(&mut runner, &[mox, thopter], mox),
        "the Mox's re-parked optional discard is held and the order restarts"
    );
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
    // CR 101.4d: paying the discard re-parks the Mox on the Root Maze /
    // Archelos ordering, so the order restarts once more.
    assert!(
        make_decision_first(&mut runner, &[mox, thopter], mox),
        "the Mox's re-parked entry ordering is held and the order restarts"
    );
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

/// CR 800.4a/f: the departing Mox owner never transfers an offered or accepted
/// discard payment to a survivor; the other Vaults entrant still completes.
#[test]
fn fu7_mox_departure_before_and_during_discard_payment_settles_surviving_entry() {
    const P2: PlayerId = PlayerId(2);
    for accept in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 0x326f);
        scenario.at_phase(Phase::PreCombatMain);
        let mox = scenario
            .add_creature_to_graveyard(P1, "Mox Diamond", 0, 0)
            .as_artifact()
            .from_oracle_text(MOX)
            .id();
        let thopter = scenario
            .add_creature_to_graveyard(P2, "Ornithopter", 0, 2)
            .as_artifact_creature()
            .from_oracle_text("Flying")
            .id();
        let forest = scenario
            .add_land_to_hand(P1, "Forest")
            .from_oracle_text("({T}: Add {G}.)")
            .id();
        let island = scenario
            .add_land_to_hand(P1, "Island")
            .from_oracle_text("({T}: Add {U}.)")
            .id();
        let vaults = scenario
            .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, VAULTS)
            .id();
        let mut runner = scenario.build();
        runner.cast(vaults).commit().try_resolve().unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ));
        let pending = runner.state().pending_replacement.as_ref().unwrap();
        assert!(pending.is_optional);
        assert_eq!(pending.proposed.affected_object_id(), Some(mox));
        assert_eq!(pending.choice_player, Some(P1));
        if accept {
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::DiscardChoice { player: P1, .. }
            ));
            assert!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .may_cost_paid
            );
        }
        assert_eq!(runner.state().objects[&mox].zone, Zone::Graveyard);
        runner.act(GameAction::Concede { player_id: P1 }).unwrap();
        let state = runner.state();
        assert!(!state.battlefield.contains(&mox));
        assert!(!state.players[P1.0 as usize].hand.contains(&forest));
        assert!(!state.players[P1.0 as usize].hand.contains(&island));
        assert_eq!(state.objects[&thopter].zone, Zone::Battlefield);
        assert_eq!(state.objects[&vaults].zone, Zone::Graveyard);
        assert!(state.pending_replacement.is_none());
        assert!(state.pending_cost_move_resume.is_none());
        assert!(!state.replacement_may_cost_paused);
        assert_settled(&runner);
    }
}

/// CR 800.4f: only the departed payer loses an unfinished payment. Another
/// player's departure leaves the exact accepted discard choice with its payer.
#[test]
fn fu7_unrelated_departure_preserves_mox_discard_and_pays_exactly_one_land() {
    let mut scenario = GameScenario::new_n_player(3, 0x3260);
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
    let mut runner = scenario.build();
    runner.cast(vaults).commit().try_resolve().unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { player: P0, .. }
    ));
    let before = runner.state().pending_replacement.clone().unwrap();
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { player: P0, .. }
    ));
    assert_eq!(runner.state().pending_replacement.as_ref(), Some(&before));
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .unwrap();
    let state = runner.state();
    assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
    assert_eq!(state.objects[&island].zone, Zone::Hand);
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(state.objects[&thopter].zone, Zone::Battlefield);
    assert_eq!(state.objects[&vaults].zone, Zone::Graveyard);
    assert!(state.pending_replacement.is_none());
    assert!(state.pending_cost_move_resume.is_none());
    assert!(!state.replacement_may_cost_paused);
    assert_settled(&runner);
}

/// CR 800.4b/f: a foreign-owned Mox cannot enter under its departed Beacon
/// controller, and the owner is not a substitute payer for its accepted cost.
#[test]
fn fu7_foreign_owned_mox_does_not_reassign_departed_beacon_payer() {
    const P2: PlayerId = PlayerId(2);
    const BEACON: &str = "Put target artifact or creature card from a graveyard onto the battlefield under your control. Shuffle Beacon of Unrest into its owner's library.";
    let mut scenario = GameScenario::new_n_player(3, 0x326b);
    scenario.at_phase(Phase::PreCombatMain);
    let mox = scenario
        .add_creature_to_graveyard(P2, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX)
        .id();
    let forest = scenario
        .add_land_to_hand(P1, "Forest")
        .from_oracle_text("({T}: Add {G}.)")
        .id();
    scenario
        .add_land_to_hand(P1, "Island")
        .from_oracle_text("({T}: Add {U}.)");
    let beacon = scenario
        .add_spell_to_hand_from_oracle(P1, "Beacon of Unrest", false, BEACON)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .cast(beacon)
        .target_object(mox)
        .commit()
        .try_resolve()
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    let pending = runner.state().pending_replacement.as_ref().unwrap();
    assert_eq!(pending.choice_player, Some(P1));
    assert!(matches!(
        &pending.proposed,
        engine::types::proposed_event::ProposedEvent::ZoneChange {
            controller_override: Some(P1),
            ..
        }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { player: P1, .. }
    ));
    assert_eq!(runner.state().objects[&forest].zone, Zone::Hand);
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    assert_eq!(runner.state().objects[&mox].zone, Zone::Graveyard);
    assert!(runner.state().players[P2.0 as usize].hand.is_empty());
    assert!(runner.state().pending_replacement.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(!runner.state().replacement_may_cost_paused);
    assert_settled(&runner);
}

/// CR 400.7: a graveyard member that left while an earlier member's
/// entry ordering was pending is never returned, and it has no battlefield
/// occurrence slot to finalize. Open the Vaults returns both Ornithopters from
/// P0's graveyard; Root Maze and Archelos make each entry an ordering choice.
///
/// Since the live scheduler (CR 616.1 + CR 101.4) the sweep consults both
/// entrants before any prompt, so the later Ornithopter is no longer
/// unconsulted when it leaves: it HOLDS its own ordering decision while P0
/// makes the first one. The property is unchanged: the held member whose
/// object moved is retired (its decision is never asked), it is not returned,
/// and the population settles.
#[test]
fn open_the_vaults_skips_a_graveyard_member_that_left_during_the_first_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let [first, later] = [0, 1].map(|_| {
        scenario
            .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
            .as_artifact_creature()
            .from_oracle_text("Flying")
            .id()
    });
    let root_maze = scenario
        .add_enchantment_from_oracle(P0, "Root Maze", ROOT_MAZE)
        .id();
    scenario.add_creature_from_oracle(P1, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, VAULTS)
        .id();
    let mut runner = scenario.build();
    runner.cast(vaults).resolve();
    assert!(
        make_decision_first(&mut runner, &[first, later], first),
        "P0 holds both Ornithopters' orderings after the sweep"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.proposed.affected_object_id()),
        Some(first),
        "the first Ornithopter's entry ordering is live"
    );
    assert!(
        runner
            .state()
            .active_batch_delivery()
            .and_then(|pending| pending.staged.as_ref())
            .and_then(|staged| staged
                .members
                .iter()
                .find(|member| member.identity.object_id == later))
            .is_some_and(|member| member.held_decision.is_some()),
        "the later Ornithopter holds its own ordering decision"
    );
    assert_eq!(runner.state().objects[&later].zone, Zone::Graveyard);

    // Stand-in for another effect exiling the held member while the first
    // Ornithopter's ordering is pending.
    engine::game::zones::move_to_zone(runner.state_mut(), later, Zone::Exile, &mut Vec::new());

    choose_source(&mut runner, root_maze, 2);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&later].zone,
        Zone::Exile,
        "the member that left is not returned"
    );
    assert_settled(&runner);
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .expect("the population settles");
}
