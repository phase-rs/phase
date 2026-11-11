//! CR 800.4j + CR 805.4a: a turn whose active player leaves the game continues to its
//! completion. With no player of the active side left, nobody is granted priority in the departed
//! player's place where a pass completes the round; with a teammate left, that teammate is the
//! active player.

use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Duration;
use engine::types::actions::GameAction;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{GameState, PendingCombatPhaseSkip, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const WOUND_REFLECTION: &str = "At the beginning of each end step, each opponent loses life equal to the life they lost this turn. (Damage causes loss of life.)";
const RELENTLESS_ASSAULT: &str = "Untap all creatures that attacked this turn. After this main phase, there is an additional combat phase followed by an additional main phase.";
const MOMENT_OF_SILENCE: &str = "Target player skips their next combat phase this turn.";
const REVENANT_PATRIARCH: &str = "When this creature enters, if {W} was spent to cast it, target player skips their next combat phase.\nThis creature can't block.";
const DOOR_TO_NOTHINGNESS: &str = "This artifact enters tapped.\n{W}{W}{U}{U}{B}{B}{R}{R}{G}{G}, {T}, Sacrifice this artifact: Target player loses the game.";

fn mana(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&t| ManaUnit::new(t, ObjectId(0), false, vec![]))
        .collect()
}

fn pass(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("pass");
}

fn concede(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::Concede { player_id: player })
        .expect("CR 104.3a: a player may concede at any time");
}

fn priority_seat(runner: &GameRunner) -> Option<PlayerId> {
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => Some(player),
        _ => None,
    }
}

fn eliminated(state: &GameState, player: PlayerId) -> bool {
    state.players[player.0 as usize].is_eliminated
}

/// Four players, P0 active at its precombat main phase at 3 life.
fn free_for_all() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 3);
    for seat in 0..4u8 {
        scenario.with_library_top(PlayerId(seat), &["Filler A", "Filler B", "Filler C"]);
    }
    scenario
}

/// What passing at every priority prompt records while `keep_going` holds: each step entered,
/// in order, and the seat of each priority prompt.
struct Walk {
    steps: Vec<Phase>,
    priority_seats: Vec<PlayerId>,
}

fn walk(runner: &mut GameRunner, mut keep_going: impl FnMut(&GameState) -> bool) -> Walk {
    let mut walked = Walk {
        steps: Vec::new(),
        priority_seats: Vec::new(),
    };
    for _ in 0..300 {
        let state = runner.state();
        if !keep_going(state) {
            break;
        }
        if walked.steps.last() != Some(&state.phase) {
            walked.steps.push(state.phase);
        }
        let Some(seat) = priority_seat(runner) else {
            break;
        };
        walked.priority_seats.push(seat);
        pass(runner);
    }
    walked
}

/// CR 800.4j + CR 117.4: once the active player has left, a pass that would hand it priority
/// resolves the top object, and then ends the step.
#[test]
fn a_departed_active_players_priority_resolves_then_ends_the_step() {
    let db = shared_card_db().expect("card db");
    let mut scenario = free_for_all();
    let opt = scenario.add_real_card(P1, "Opt", Zone::Hand, db);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(&[ManaType::Blue, ManaType::Red]));
    let mut runner = scenario.build();

    pass(&mut runner);
    runner.cast(opt).commit();
    runner.cast(bolt).target_player(P0).commit();
    for _ in 0..4 {
        pass(&mut runner);
    }
    assert!(eliminated(runner.state(), P0));
    assert_eq!(priority_seat(&runner), Some(P1));
    assert_eq!(runner.stack_names(), ["Opt"]);

    for _ in 0..3 {
        pass(&mut runner);
    }
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ScryChoice { player: P1, .. }
    ));
    let hand = runner.state().players[1].hand.len();
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("scry");
    assert_eq!(priority_seat(&runner), Some(P1));
    assert_eq!(runner.state().players[1].hand.len(), hand + 1);

    for _ in 0..3 {
        pass(&mut runner);
    }
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(priority_seat(&runner), Some(P1));
}

/// CR 800.4a + CR 117.4: a player who passed and then left is not among the players who pass
/// in succession.
#[test]
fn a_departed_players_pass_does_not_complete_the_round() {
    let mut runner = free_for_all().build();
    pass(&mut runner);
    pass(&mut runner);
    concede(&mut runner, P0);
    pass(&mut runner);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(priority_seat(&runner), Some(P3));
    pass(&mut runner);
    assert_ne!(runner.state().phase, Phase::PreCombatMain);
}

/// An Archenemy game with P1, of the archenemy's opponents' team, active at its precombat main
/// phase with priority.
fn archenemy(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.turn_number = 2;
    state.active_player = P1;
    state.phase = Phase::PreCombatMain;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    state.priority_passes.clear();
    runner
}

fn archenemy_board() -> GameScenario {
    GameScenario::new_with_format(FormatConfig::archenemy(), 4, 7)
}

/// CR 805.5b: a team's pass stays the team's when the player who made it leaves and a teammate
/// remains.
#[test]
fn a_team_keeps_its_pass_when_its_representative_leaves() {
    let mut runner = archenemy(archenemy_board());
    pass(&mut runner);
    assert_eq!(priority_seat(&runner), Some(P0));
    assert_eq!(
        runner
            .state()
            .priority_passes
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [P1]
    );
    concede(&mut runner, P1);
    pass(&mut runner);
    assert_ne!(runner.state().phase, Phase::PreCombatMain);
}

/// CR 805.4a + CR 805.8: "skips their next combat phase" recorded for one member of the active
/// team (Blinding Angel's trigger) is the team's after its engine active player leaves.
#[test]
fn a_team_skips_its_combat_after_its_active_player_leaves() {
    for leaves in [false, true] {
        let mut runner = archenemy(archenemy_board());
        runner
            .state_mut()
            .pending_combat_phase_skips
            .push(PendingCombatPhaseSkip {
                player: P2,
                duration: None,
            });
        if leaves {
            concede(&mut runner, P1);
        }
        walk(&mut runner, |state| state.phase == Phase::PreCombatMain);
        assert_eq!(
            runner.state().phase,
            Phase::PostCombatMain,
            "leaves={leaves}"
        );
        assert!(runner.state().pending_combat_phase_skips.is_empty());
    }
}

/// CR 800.4j + CR 603.2b: the departed active player's turn reaches its end step, and an "at
/// the beginning of each end step" trigger (Wound Reflection) resolves in it.
#[test]
fn a_departed_active_players_turn_reaches_its_end_step() {
    let db = shared_card_db().expect("card db");
    let mut scenario = free_for_all();
    scenario.add_enchantment_from_oracle(P1, "Wound Reflection", WOUND_REFLECTION);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    let second = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(&[ManaType::Red, ManaType::Red]));
    let mut runner = scenario.build();

    pass(&mut runner);
    runner.cast(bolt).target_player(P0).commit();
    for _ in 0..4 {
        pass(&mut runner);
    }
    assert!(eliminated(runner.state(), P0));
    runner.cast(second).target_player(P2).commit();
    for _ in 0..3 {
        pass(&mut runner);
    }
    assert_eq!(runner.state().players[2].life, 17);

    let mut at_trigger = None;
    let walked = walk(&mut runner, |state| {
        if state.phase == Phase::End && !state.stack.is_empty() {
            at_trigger.get_or_insert(state.players[2].life);
        }
        state.active_player == P0
    });
    assert_eq!(
        walked.steps,
        [
            Phase::PreCombatMain,
            Phase::BeginCombat,
            Phase::EndCombat,
            Phase::PostCombatMain,
            Phase::End
        ]
    );
    assert_eq!(at_trigger, Some(17));
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().players[2].life, 14);
    assert!(!walked.priority_seats.contains(&P0));
}

/// CR 800.4j + CR 500.8: the phases a resolved spell (Relentless Assault) added to the turn run
/// after its active player leaves.
#[test]
fn a_departed_active_players_turn_runs_its_added_phases() {
    for leaves in [false, true] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        for seat in 0..4u8 {
            scenario.with_library_top(PlayerId(seat), &["Filler A", "Filler B", "Filler C"]);
        }
        let assault = scenario
            .add_spell_to_hand_from_oracle(P0, "Relentless Assault", false, RELENTLESS_ASSAULT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 2,
            })
            .id();
        scenario.with_mana_pool(P0, mana(&[ManaType::Red; 4]));
        let mut runner = scenario.build();
        runner.cast(assault).commit();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        for _ in 0..4 {
            pass(&mut runner);
        }
        if leaves {
            concede(&mut runner, P0);
        }
        let walked = walk(&mut runner, |state| state.active_player == P0);
        assert_eq!(
            walked.steps,
            [
                Phase::PreCombatMain,
                Phase::BeginCombat,
                Phase::EndCombat,
                Phase::PostCombatMain,
                Phase::BeginCombat,
                Phase::EndCombat,
                Phase::PostCombatMain,
                Phase::End
            ],
            "leaves={leaves}"
        );
        assert_eq!(
            (runner.state().active_player, runner.state().phase),
            (P1, Phase::Upkeep)
        );
    }
}

/// CR 800.4j + CR 616.1 + CR 500.8: when the active player leaves with its choice between the
/// skips of the combat phase an added phase begins with unanswered, that combat begins and the
/// added phases run in order.
#[test]
fn a_departed_active_players_unanswered_skip_choice_keeps_its_added_phases_in_order() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..4u8 {
        scenario.with_library_top(PlayerId(seat), &["Filler A", "Filler B", "Filler C"]);
    }
    let patriarch = scenario
        .add_creature_to_hand_from_oracle(P0, "Revenant Patriarch", 4, 3, REVENANT_PATRIARCH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .id();
    let moment = scenario
        .add_spell_to_hand_from_oracle(P0, "Moment of Silence", true, MOMENT_OF_SILENCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let assault = scenario
        .add_spell_to_hand_from_oracle(P0, "Relentless Assault", false, RELENTLESS_ASSAULT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    let cast = |runner: &mut GameRunner, spell: ObjectId, pool: &[ManaType]| {
        for unit in mana(pool) {
            runner.state_mut().players[0].mana_pool.add(unit);
        }
        runner.cast(spell).target_player(P0).resolve();
    };
    cast(
        &mut runner,
        patriarch,
        &[
            ManaType::White,
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
        ],
    );
    cast(&mut runner, moment, &[ManaType::White]);
    cast(&mut runner, assault, &[ManaType::Red; 4]);
    assert_eq!(runner.state().pending_combat_phase_skips.len(), 2);

    for _ in 0..4 {
        pass(&mut runner);
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let mut steps = vec![runner.state().phase];
    concede(&mut runner, P0);
    steps.extend(walk(&mut runner, |state| state.active_player == P0).steps);
    steps.dedup();
    assert_eq!(
        steps,
        [
            Phase::PreCombatMain,
            Phase::BeginCombat,
            Phase::EndCombat,
            Phase::PostCombatMain,
            Phase::BeginCombat,
            Phase::EndCombat,
            Phase::PostCombatMain,
            Phase::End
        ]
    );
    assert_eq!(
        (runner.state().active_player, runner.state().phase),
        (P1, Phase::Upkeep)
    );
}

/// CR 805.4a + CR 805.8 + CR 616.1: when the engine's active player leaves a shared-team turn
/// with its choice between the team's combat skips unanswered, the teammate now active makes it.
#[test]
fn a_remaining_teammate_makes_the_departed_players_skip_choice() {
    let mut runner = archenemy(archenemy_board());
    // The skips Blinding Angel's trigger and Moment of Silence record for P2.
    for duration in [None, Some(Duration::UntilEndOfTurn)] {
        runner
            .state_mut()
            .pending_combat_phase_skips
            .push(PendingCombatPhaseSkip {
                player: P2,
                duration,
            });
    }
    while priority_seat(&runner).is_some() {
        pass(&mut runner);
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    concede(&mut runner, P1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P2, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("CR 616.1: the affected player chooses");
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(runner.state().pending_combat_phase_skips.len(), 1);
}

/// CR 805.4a + CR 805.9 + CR 508.1: when the engine's active player leaves a shared-team turn
/// and a teammate remains, the teammate is the active player and declares the team's attack.
#[test]
fn a_remaining_teammate_is_the_active_player() {
    for leaves in [false, true] {
        let db = shared_card_db().expect("card db");
        let mut scenario = archenemy_board();
        let p2_bears = scenario.add_real_card(P2, "Grizzly Bears", Zone::Battlefield, db);
        let p1_bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        let mut runner = archenemy(scenario);
        if leaves {
            concede(&mut runner, P1);
            assert!(eliminated(runner.state(), P1));
            assert_eq!(runner.state().active_player, P2);
        }
        let declarer = if leaves { P2 } else { P1 };
        for _ in 0..50 {
            if priority_seat(&runner).is_none() {
                break;
            }
            assert_eq!(runner.state().active_player, declarer);
            pass(&mut runner);
        }
        let WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!("{:?}", runner.state().waiting_for);
        };
        assert_eq!(player, declarer, "leaves={leaves}");
        let mut expected = if leaves {
            vec![p2_bears]
        } else {
            vec![p2_bears, p1_bears]
        };
        let mut valid = valid_attacker_ids;
        expected.sort_unstable();
        valid.sort_unstable();
        assert_eq!(valid, expected);

        let declaration = GameAction::DeclareAttackers {
            attacks: vec![(p2_bears, AttackTarget::Player(P0))],
            bands: vec![],
        };
        if leaves {
            let mut clone = runner.state().clone();
            assert!(matches!(
                engine::game::engine::apply(&mut clone, P1, declaration.clone()),
                Err(EngineError::WrongPlayer)
            ));
        }
        engine::game::engine::apply(runner.state_mut(), declarer, declaration)
            .expect("the active player declares");
        walk(&mut runner, |state| state.turn_number == 2);
        assert_eq!(runner.state().players[0].life, 38);
        assert_eq!(
            (runner.state().turn_number, runner.state().active_player),
            (3, P0)
        );
    }
}

/// CR 805.4a + CR 805.9 in a two-Horde game: when the active Horde seat leaves (Door to
/// Nothingness) and the other remains, that seat is the active player and declares the Horde's
/// attack, and the turn hands over to the survivors.
#[test]
fn the_remaining_horde_seat_is_the_active_player() {
    for leaves in [false, true] {
        let db = shared_card_db().expect("card db");
        let mut scenario = GameScenario::new_with_format(
            FormatConfig::horde(ChallengeDeck::LotrTwoTowersHorde.default_ruleset()),
            4,
            42,
        );
        let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        let door = scenario
            .add_artifact_from_oracle(P2, "Door to Nothingness", DOOR_TO_NOTHINGNESS)
            .id();
        use ManaType::{Black, Blue, Green, Red, White};
        scenario.with_mana_pool(
            P2,
            mana(&[
                White, White, Blue, Blue, Black, Black, Red, Red, Green, Green,
            ]),
        );
        let mut runner = scenario.build();
        assert_eq!(runner.state().horde_seats, [P0, P1]);
        {
            let state = runner.state_mut();
            state.turn_number = 3;
            state.active_player = P0;
            state.phase = Phase::PreCombatMain;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
            state.priority_passes.clear();
        }
        pass(&mut runner);
        assert_eq!(priority_seat(&runner), Some(P2));
        if leaves {
            runner.activate(door, 0).target_player(P0).resolve();
            assert_eq!(runner.state().players[2].mana_pool.total(), 0);
            assert!(!runner.state().battlefield.contains(&door));
            assert!(eliminated(runner.state(), P0));
            assert!(!eliminated(runner.state(), P1));
            assert_eq!(runner.state().active_player, P1);
        }
        let declarer = if leaves { P1 } else { P0 };
        let mut prompted = Vec::new();
        let mut declared = None;
        for _ in 0..400 {
            if runner.state().turn_number != 3 {
                break;
            }
            let waiting_for = runner.state().waiting_for.clone();
            prompted.extend(waiting_for.acting_player());
            match waiting_for {
                WaitingFor::Priority { .. } => pass(&mut runner),
                WaitingFor::DeclareAttackers {
                    player,
                    valid_attacker_ids,
                    ..
                } => {
                    declared = Some((player, valid_attacker_ids));
                    runner
                        .act(GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        })
                        .expect("no attackers");
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(declared, Some((declarer, vec![bears])), "leaves={leaves}");
        if leaves {
            assert!(!prompted.contains(&P0));
        }
        assert_eq!(
            (runner.state().turn_number, runner.state().active_player),
            (4, P2)
        );
    }
}
