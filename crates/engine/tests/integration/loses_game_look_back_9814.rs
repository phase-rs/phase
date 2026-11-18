//! #9814: "when a player loses the game" triggers look back in time (CR 603.10f). Printed cards
//! only. CR 603.3a: a
//! trigger's controller is its source's controller when it triggered. CR 800.4a: a leaving
//! player's objects leave and their control effects end. CR 800.4d: a trigger a departed player
//! would control isn't put on the stack. CR 104.3a/b/e: concession, 0 life, an effect's loss.

use std::collections::BTreeSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{ChoiceType, ChosenAttribute, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, LossReason};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType::{Black as B, Blue as U, Green as G, Red as R, White as W};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
/// Zenos yae Galvus's mana value: Dominate's X.
const ZENOS_MV: u32 = 5;

struct Board {
    runner: GameRunner,
    zenos: ObjectId,
    hand: Vec<ObjectId>,
    /// On the battlefield, untapped.
    extras: Vec<ObjectId>,
}

/// Four players in P0's main phase; `zenos_owner` owns Zenos yae Galvus on the battlefield.
fn board(zenos_owner: PlayerId, hand: &[(PlayerId, &str)], extras: &[(PlayerId, &str)]) -> Board {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let zenos = scenario.add_real_card(zenos_owner, "Zenos yae Galvus", Zone::Battlefield, db);
    let mut add = |cards: &[(PlayerId, &str)], zone| -> Vec<ObjectId> {
        let mut add =
            |&(player, name): &(PlayerId, &str)| scenario.add_real_card(player, name, zone, db);
        cards.iter().map(&mut add).collect()
    };
    let (hand, extras) = (add(hand, Zone::Hand), add(extras, Zone::Battlefield));
    for player in [P0, P1, P2, P3].repeat(5) {
        scenario.add_card_to_library_top(player, "Plains");
    }
    let mut runner = scenario.build();
    for id in &extras {
        runner.state_mut().objects.get_mut(id).unwrap().tapped = false;
    }
    let has_back_face = runner.state().objects[&zenos].back_face.is_some();
    assert!(has_back_face, "reach: Zenos has its Shinryu face");
    Board {
        runner,
        zenos,
        hand,
        extras,
    }
}

fn winner(runner: &GameRunner) -> Option<Option<PlayerId>> {
    match &runner.state().waiting_for {
        WaitingFor::GameOver { winner } => Some(*winner),
        _ => None,
    }
}

fn chosen(runner: &GameRunner, zenos: ObjectId) -> Vec<ChosenAttribute> {
    runner.state().objects[&zenos].chosen_attributes.clone()
}

/// Fill `player`'s emptied pool with exactly `mana`, so no payment spends another's colour.
fn fund(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    pool.clear();
    for &mana in mana {
        let unit = ManaUnit::new(mana, ObjectId(0), false, vec![]);
        let _ = runner.state_mut().add_mana_to_pool(player, unit);
    }
}

/// Resolve the stack to empty (or the game's end), ordering any simultaneous triggers.
fn settle(runner: &mut GameRunner) {
    for _ in 0..60 {
        match runner.state().waiting_for {
            WaitingFor::GameOver { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            ref other => panic!("unexpected wait {other:?}"),
        }
    }
    panic!("the stack did not settle");
}

/// P0 casts Moonmist ("Transform all Humans.") to turn Zenos into Shinryu, whose "As this
/// creature transforms into Shinryu, choose an opponent." prompt must offer `choose`, chosen.
fn moonmist(runner: &mut GameRunner, mist: ObjectId, zenos: ObjectId, choose: PlayerId) {
    fund(runner, P0, &[G, G]);
    runner.cast(mist).resolve();
    runner.advance_until_stack_empty();
    let transformed = runner.state().objects[&zenos].transformed;
    assert!(transformed, "reach: transformed");
    let (waiting, choice) = (&runner.state().waiting_for, choose.0.to_string());
    let asked = matches!(waiting, WaitingFor::NamedChoice {
        choice_type: ChoiceType::Opponent { .. }, options, ..
    } if options.contains(&choice));
    assert!(asked, "no {choice} opponent prompt: {waiting:?}");
    let choose = GameAction::ChooseOption { choice };
    runner.act(choose).expect("choose");
    settle(runner);
}

/// `caster`, holding priority, casts Dominate with X = Zenos's mana value on it.
fn dominate(runner: &mut GameRunner, dominate: ObjectId, zenos: ObjectId, caster: PlayerId) {
    let holds = runner.state().waiting_for == WaitingFor::Priority { player: caster };
    assert!(holds, "reach: the caster holds priority");
    fund(runner, caster, &[U, U, B, B, B, B, B, B]);
    let cast = runner.cast(dominate).x(ZENOS_MV);
    cast.target_object(zenos).resolve();
    settle(runner);
    let controller = runner.state().objects[&zenos].controller;
    assert_eq!(controller, caster, "reach: stolen");
}

#[derive(Clone, Copy)]
enum Loss {
    /// CR 104.3b: 0 life, lost at the next state-based-action check.
    Life,
    /// CR 104.3a.
    Concede,
    /// CR 104.3e: P0 activates Door to Nothingness (`extras[0]`) targeting the loser.
    Door,
}

/// `loser` loses by `loss` while two or more others are in, so only a trigger can end the
/// game; then the stack settles.
fn lose(board: &mut Board, loser: PlayerId, loss: Loss) {
    let runner = &mut board.runner;
    let in_game = |runner: &GameRunner| -> Vec<PlayerId> {
        let players = runner
            .state()
            .players
            .iter()
            .filter(|player| !player.is_eliminated);
        players
            .map(|player| player.id)
            .filter(|p| *p != loser)
            .collect()
    };
    let others_in = in_game(runner);
    assert!(others_in.len() >= 2, "reach: two others in");
    match loss {
        Loss::Life => {
            runner.state_mut().players[loser.0 as usize].life = 0;
            runner.act(GameAction::PassPriority).expect("pass");
        }
        Loss::Concede => {
            let concede = GameAction::Concede { player_id: loser };
            runner.act(concede).expect("concede");
        }
        Loss::Door => {
            fund(runner, P0, &[W, W, U, U, B, B, R, R, G, G]);
            let door = runner.activate(board.extras[0], 0);
            let door = door.target_player(loser).resolve();
            // Its driver also resolves what the loss triggered: the others may have left
            // since, but only by a win.
            let lost: Vec<_> = (door.events().iter())
                .filter_map(|event| match event {
                    GameEvent::PlayerLost { player_id, reasons } => Some((*player_id, reasons)),
                    _ => None,
                })
                .collect();
            let why = |player| {
                let reason = if player == loser {
                    LossReason::EffectStatedLoss
                } else {
                    LossReason::OpponentWon
                };
                BTreeSet::from([reason])
            };
            let door_loss = lost.iter().any(|(player, _)| *player == loser);
            assert!(door_loss, "reach: Door's loss");
            let each = lost
                .iter()
                .all(|(player, reasons)| **reasons == why(*player));
            assert!(each, "reach: why each player left: {lost:?}");
        }
    }
    let loser_out = runner.state().players[loser.0 as usize].is_eliminated;
    assert!(loser_out, "reach: {loser:?} lost");
    if !matches!(loss, Loss::Door) {
        assert_eq!(in_game(runner), others_in, "reach: the others are still in");
    }
    settle(runner);
}

/// R1: P0 steals P2's Zenos with Dominate, turns it to Shinryu with Moonmist and chooses P2.
/// When P2 loses, Shinryu leaves the game with its owner (CR 800.4a), but the ability looks
/// back (CR 603.10f): it existed under P0 immediately before, so P0 wins.
fn stolen_shinryu_owner_loses(loss: Loss) {
    let door = [(P0, "Door to Nothingness")];
    let mut board = board(P2, &[(P0, "Dominate"), (P0, "Moonmist")], &door);
    let (zenos, steal, mist) = (board.zenos, board.hand[0], board.hand[1]);
    dominate(&mut board.runner, steal, zenos, P0);
    moonmist(&mut board.runner, mist, zenos, P2);
    assert_eq!(
        board.runner.state().objects[&zenos].owner,
        P2,
        "reach: P2 owns it"
    );
    assert_eq!(chosen(&board.runner, zenos), [ChosenAttribute::Player(P2)]);
    lose(&mut board, P2, loss);
    let won = winner(&board.runner);
    assert_eq!(
        won,
        Some(Some(P0)),
        "CR 603.10f: the stolen Shinryu saw its owner lose"
    );
}

/// R2: P0's Shinryu (chosen P2) is stolen by P2. When P2 loses, the ability looks back: it
/// triggered under P2 (CR 603.3a), who has left, so it isn't put on the stack (CR 800.4d).
/// Shinryu returning to P0 (CR 800.4a) doesn't make P0 its controller when it triggered.
fn shinryu_stolen_by_the_loser(loss: Loss) {
    let mut board = board(P0, &[(P0, "Moonmist"), (P2, "Dominate")], &[]);
    let (zenos, mist, steal) = (board.zenos, board.hand[0], board.hand[1]);
    moonmist(&mut board.runner, mist, zenos, P2);
    for _ in 0..2 {
        board.runner.act(GameAction::PassPriority).expect("pass");
    }
    dominate(&mut board.runner, steal, zenos, P2);
    assert_eq!(chosen(&board.runner, zenos), [ChosenAttribute::Player(P2)]);
    lose(&mut board, P2, loss);
    let shinryu = &board.runner.state().objects[&zenos];
    let back = (shinryu.zone, shinryu.controller);
    assert_eq!(
        back,
        (Zone::Battlefield, P0),
        "CR 800.4a: the control effect ended"
    );
    let won = winner(&board.runner);
    assert_eq!(
        won, None,
        "CR 603.3a + CR 800.4d: the trigger was P2's, who has left"
    );
}

/// R5, CR 104.3a / CR 104.3e: P0's own Shinryu (chosen P2) wins when P2 concedes or an
/// effect makes P2 lose, not only on a state-based-action loss.
fn own_shinryu(loss: Loss) {
    let mut board = board(P0, &[(P0, "Moonmist")], &[(P0, "Door to Nothingness")]);
    let (zenos, mist) = (board.zenos, board.hand[0]);
    moonmist(&mut board.runner, mist, zenos, P2);
    lose(&mut board, P2, loss);
    assert_eq!(winner(&board.runner), Some(Some(P0)));
}

macro_rules! rows {
    ($($name:ident: $row:ident($loss:ident);)*) => {
        $(#[test] fn $name() { $row(Loss::$loss) })*
    };
}

rows! {
    stolen_shinryu_wins_when_its_owner_loses_by_life: stolen_shinryu_owner_loses(Life);
    stolen_shinryu_wins_when_its_owner_concedes: stolen_shinryu_owner_loses(Concede);
    stolen_shinryu_wins_when_an_effect_makes_its_owner_lose: stolen_shinryu_owner_loses(Door);
    shinryu_stolen_by_the_chosen_player_does_not_win_when_they_lose_by_life:
        shinryu_stolen_by_the_loser(Life);
    shinryu_stolen_by_the_chosen_player_does_not_win_when_they_concede:
        shinryu_stolen_by_the_loser(Concede);
    shinryu_wins_when_the_chosen_player_concedes: own_shinryu(Concede);
    shinryu_wins_when_an_effect_makes_the_chosen_player_lose: own_shinryu(Door);
}

/// R3, class sibling: P0 takes P2's Share the Spoils with Confiscate. When P2 concedes, the
/// enchantment leaves with its owner, but "whenever an opponent loses the game" looks back
/// and triggers under P0, exiling the top card of P0's library.
#[test]
fn stolen_share_the_spoils_triggers_when_its_owner_concedes() {
    let mut board = board(P0, &[(P0, "Confiscate")], &[(P2, "Share the Spoils")]);
    let spoils = board.extras[0];
    fund(&mut board.runner, P0, &[U, U, B, B, B, B]);
    board
        .runner
        .cast(board.hand[0])
        .target_object(spoils)
        .resolve();
    settle(&mut board.runner);
    let state = board.runner.state();
    let (controller, owner) = (
        state.objects[&spoils].controller,
        state.objects[&spoils].owner,
    );
    assert_eq!((controller, owner), (P0, P2), "reach: Confiscate took it");
    let library_before = state.players[0].library.len();
    lose(&mut board, P2, Loss::Concede);
    let library = board.runner.state().players[0].library.len();
    assert_eq!(
        library,
        library_before - 1,
        "CR 603.10f: it exiled P0's top card"
    );
}

/// Answer the pending "choose a player"/"choose an opponent" prompt with `player`.
fn answer_player(runner: &mut GameRunner, player: PlayerId) {
    let waiting = &runner.state().waiting_for;
    let asked = matches!(waiting, WaitingFor::NamedChoice { options, .. }
        if options.contains(&player.0.to_string()));
    assert!(asked, "reach: a prompt offering {player:?}: {waiting:?}");
    let choice = player.0.to_string();
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("choose");
}

/// #11062 review: three players. P0 casts True-Name Nemesis choosing P1 ("As this creature enters,
/// choose a player. / This creature has protection from the chosen player."), casts Mirrorweave
/// ("Each other creature becomes a copy of target nonlegendary creature until end of turn.") on
/// Triarch Stalker, and at combat both Targeting Relays ("At the beginning of combat on your turn,
/// choose an opponent.") choose P2. CR 607.2d + CR 607.5: the copied Relay's choice belongs to the
/// copied pair, so after cleanup Nemesis still has protection from P1, not from P2.
#[test]
fn copied_targeting_relay_choice_leaves_nemesis_protection_from_p1() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let real = |s: &mut GameScenario, name, zone| s.add_real_card(P0, name, zone, db);
    let nemesis = real(&mut scenario, "True-Name Nemesis", Zone::Hand);
    let weave = real(&mut scenario, "Mirrorweave", Zone::Hand);
    let stalker = real(&mut scenario, "Triarch Stalker", Zone::Battlefield);
    let sources = [P1, P2].map(|player| scenario.add_creature(player, "Source", 1, 1).id());
    for player in [P0, P1, P2].repeat(5) {
        scenario.add_card_to_library_top(player, "Plains");
    }
    let mut runner = scenario.build();
    fund(&mut runner, P0, &[U, U, U]);
    runner.cast(nemesis).resolve();
    answer_player(&mut runner, P1);
    settle(&mut runner);
    fund(&mut runner, P0, &[W, W, W, W]);
    runner.cast(weave).target_object(stalker).resolve();
    settle(&mut runner);
    let copy = &runner.state().objects[&nemesis].name;
    assert_eq!(
        copy, "Triarch Stalker",
        "reach: Nemesis is a copy of Stalker"
    );
    let mut answered = 0;
    while runner.state().active_player == P0 {
        match runner.state().waiting_for {
            WaitingFor::NamedChoice { .. } => {
                answer_player(&mut runner, P2);
                answered += 1;
            }
            WaitingFor::DeclareAttackers { .. } => {
                let none = GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                };
                runner.act(none).expect("no attackers");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => drop(runner.act(GameAction::PassPriority).expect("pass")),
        }
    }
    assert_eq!(answered, 2, "reach: both Targeting Relays chose P2");
    let state = runner.state();
    assert_eq!(
        state.objects[&nemesis].name, "True-Name Nemesis",
        "reach: the copy ended"
    );
    let targetable_by = |source: ObjectId| {
        let (any, controller) = (TargetFilter::Any, state.objects[&source].controller);
        find_legal_targets(state, &any, controller, source).contains(&TargetRef::Object(nemesis))
    };
    assert!(!targetable_by(sources[0]), "CR 702.16b: protection from P1");
    assert!(targetable_by(sources[1]), "no protection from P2");
}
