//! CR 104.3: every player who leaves the game by losing emits `PlayerLost` with why, and
//! "whenever a player loses the game" observers see concessions and effect losses.

use std::collections::BTreeSet;

use engine::game::effects::attach::attach_to_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, LossReason};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const CURSE_OF_VENGEANCE: &str = "Enchant player\nWhenever enchanted player casts a spell, put a spite counter on this Aura.\nWhen enchanted player loses the game, you gain X life and draw X cards, where X is the number of spite counters on this Aura.";

struct Board {
    runner: GameRunner,
    tyrant: ObjectId,
    door: ObjectId,
}

fn add_curse(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_creature(P0, "Curse of Vengeance", 0, 0);
    builder.as_enchantment();
    builder.with_subtypes(vec!["Aura", "Curse"]);
    builder.from_oracle_text(CURSE_OF_VENGEANCE);
    builder.id()
}

fn enchant(runner: &mut GameRunner, curse: ObjectId, player: PlayerId, spite: u32) {
    attach_to_player(runner.state_mut(), curse, player);
    runner
        .state_mut()
        .objects
        .get_mut(&curse)
        .unwrap()
        .counters
        .insert(CounterType::Generic("spite".to_string()), spite);
}

/// Three players; P0 controls Blood Tyrant, an untapped Door to Nothingness with its mana, a
/// Curse of Vengeance with 2 spite counters on P2 and one with 3 on P1.
fn three_player_board() -> Board {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let tyrant = scenario.add_real_card(P0, "Blood Tyrant", Zone::Battlefield, db);
    let door = scenario.add_real_card(P0, "Door to Nothingness", Zone::Battlefield, db);
    let loser_curse = add_curse(&mut scenario);
    let survivor_curse = add_curse(&mut scenario);
    for _ in 0..10 {
        for player in [P0, P1, P2] {
            scenario.add_card_to_library_top(player, "Plains");
        }
    }
    use ManaType::{Black, Blue, Green, Red, White};
    scenario.with_mana_pool(
        P0,
        [
            White, White, Blue, Blue, Black, Black, Red, Red, Green, Green,
        ]
        .into_iter()
        .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    enchant(&mut runner, loser_curse, P2, 2);
    enchant(&mut runner, survivor_curse, P1, 3);
    runner.state_mut().objects.get_mut(&door).unwrap().tapped = false;
    Board {
        runner,
        tyrant,
        door,
    }
}

fn losses(events: &[GameEvent]) -> Vec<(PlayerId, BTreeSet<LossReason>)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerLost { player_id, reasons } => Some((*player_id, reasons.clone())),
            _ => None,
        })
        .collect()
}

fn position(events: &[GameEvent], wanted: impl Fn(&GameEvent) -> bool) -> usize {
    events.iter().position(wanted).expect("event in the batch")
}

fn resolve_triggers(runner: &mut GameRunner) {
    for _ in 0..60 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            ref other => panic!("unexpected wait {other:?}"),
        }
    }
    panic!("triggers did not settle");
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Blood Tyrant ("Whenever a player loses the game, put five +1/+1 counters") and the loser's
/// Curse of Vengeance (X = 2) pay out; the Curse on P1, who did not lose, stays silent.
fn assert_observers_paid(mut board: Board, life_before: i32, hand_before: usize) {
    resolve_triggers(&mut board.runner);
    let p0 = &board.runner.state().players[0];
    assert_eq!(plus_one_counters(&board.runner, board.tyrant), 5);
    assert_eq!(p0.life, life_before + 2);
    assert_eq!(p0.hand.len(), hand_before + 2);
}

/// CR 104.3a: the conceding player loses the game.
#[test]
fn a_concession_records_its_reason_and_fires_loss_observers() {
    let mut board = three_player_board();
    let life_before = board.runner.state().players[0].life;
    let hand_before = board.runner.state().players[0].hand.len();

    let events = board
        .runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("concede")
        .events;

    assert_eq!(
        losses(&events),
        [(P2, BTreeSet::from([LossReason::Conceded]))]
    );
    let lost = position(
        &events,
        |event| matches!(event, GameEvent::PlayerLost { player_id, .. } if *player_id == P2),
    );
    let left = position(&events, |event| {
        *event == GameEvent::PlayerEliminated { player_id: P2 }
    });
    assert_observers_paid(board, life_before, hand_before);
    assert!(lost < left);
}

/// CR 810.8b: a conceding player's team leaves the game and loses it; CR 104.2c: the other team
/// wins.
#[test]
fn a_two_headed_giant_concession_records_each_head() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let events = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("concede")
        .events;

    let conceded = BTreeSet::from([LossReason::Conceded]);
    assert_eq!(losses(&events), [(P2, conceded.clone()), (P3, conceded)]);
    let state = runner.state();
    assert!(state.players[2].is_eliminated && state.players[3].is_eliminated);
    assert_eq!(state.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
}

/// CR 104.3e: Door to Nothingness's "Target player loses the game."
#[test]
fn an_effect_loss_records_its_reason_and_fires_loss_observers() {
    let mut board = three_player_board();
    let life_before = board.runner.state().players[0].life;
    let hand_before = board.runner.state().players[0].hand.len();

    let events = board
        .runner
        .activate(board.door, 0)
        .target_player(P2)
        .resolve()
        .events()
        .to_vec();

    assert_eq!(
        losses(&events),
        [(P2, BTreeSet::from([LossReason::EffectStatedLoss]))]
    );
    assert_observers_paid(board, life_before, hand_before);
}
