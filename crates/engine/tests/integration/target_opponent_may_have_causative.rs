//! The "target opponent may have <causee> <predicate>" class — CR 115.1 + CR
//! 118.12 + CR 608.2d: exactly one opponent is declared as a target as the
//! spell or ability is put on the stack (CR 115.1a / CR 115.1d, CR 601.2c),
//! only that player makes the may-choice, and the causee ("you" = the
//! controller, CR 109.5; or the card itself) performs the predicate. In the
//! predicate and the body, "they" / "them" / "that player" / "the player" name
//! the declared target; no other opponent is asked or affected.
//!
//! Cards (printed, added through `add_real_card` on the curated card
//! database): Bane, Lord of Darkness; Combustible Gearhulk; Palantír of
//! Orthanc; Terrapact Intimidator; Risk Factor. Tell Me What You See, the
//! sixth member, lives in `tell_me_what_you_see_etb.rs`. Printed costs are
//! made payable by adding basic lands; the other hand and library cards are
//! textless builder fillers.
//!
//! Every row seats three players and targets the SECOND opponent, P2, so
//! neither seat order (P1 first) nor a controller fallback (P0) can satisfy an
//! assertion. The Leyline of Sanctity rows give opponents hexproof (CR
//! 702.11c): with no legal target the trigger is removed (CR 603.3d) and a
//! spell cannot be cast (CR 601.2c); with one legal target that target is
//! chosen without a prompt.

use std::collections::VecDeque;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

/// Every prompt the drive raised, in order, with its actor.
#[derive(Debug, Default)]
struct Record {
    /// `legal_targets` of each target prompt, in prompt order.
    target_prompts: Vec<Vec<TargetRef>>,
    /// The player asked each `OptionalEffectChoice`.
    optional_actors: Vec<PlayerId>,
    /// The player of each `ScryChoice`.
    scry_actors: Vec<PlayerId>,
    /// `(player, cards)` of each `EffectZoneChoice`.
    zone_choices: Vec<(PlayerId, Vec<ObjectId>)>,
}

/// The planned answers, consumed in prompt order. A prompt with no planned
/// answer left panics, so an unplanned prompt is never silently answered.
#[derive(Default)]
struct Answers {
    targets: VecDeque<TargetRef>,
    optional: VecDeque<bool>,
    zone_picks: VecDeque<Vec<ObjectId>>,
}

impl Answers {
    fn new(targets: &[TargetRef], optional: &[bool]) -> Self {
        Answers {
            targets: targets.iter().cloned().collect(),
            optional: optional.iter().copied().collect(),
            zone_picks: VecDeque::new(),
        }
    }
}

/// Answer every prompt from `answers` until the stack is empty at a priority
/// window.
fn drive(runner: &mut GameRunner, mut answers: Answers) -> Record {
    let mut record = Record::default();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                record
                    .target_prompts
                    .push(target_slots[selection.current_slot].legal_targets.clone());
                let target = answers
                    .targets
                    .pop_front()
                    .unwrap_or_else(|| panic!("unplanned target prompt: {record:?}"));
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target),
                    })
                    .expect("choose the planned target");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                record.optional_actors.push(player);
                let accept = answers
                    .optional
                    .pop_front()
                    .unwrap_or_else(|| panic!("unplanned optional prompt: {record:?}"));
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the optional prompt");
            }
            WaitingFor::ScryChoice { player, cards } => {
                record.scry_actors.push(player);
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("keep the scried cards on top");
            }
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                record.zone_choices.push((player, cards));
                let pick = answers
                    .zone_picks
                    .pop_front()
                    .unwrap_or_else(|| panic!("unplanned zone choice: {record:?}"));
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("choose the planned cards");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return record;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the stack resolves");
            }
            other => panic!("unexpected prompt {other:?}; record {record:?}"),
        }
    }
    panic!("the stack did not resolve: {record:?}");
}

/// A three-player game in P0's precombat main phase with four library cards
/// per seat (so every draw, mill and scry has cards).
fn three_players() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..3u8 {
        for index in 0..4 {
            scenario.add_spell_to_library_top(
                PlayerId(seat),
                &format!("Library {seat}-{index}"),
                false,
            );
        }
    }
    scenario
}

/// CR 702.11c: Leyline of Sanctity gives each listed opponent hexproof.
fn add_leylines(scenario: &mut GameScenario, players: &[PlayerId], db: &CardDatabase) {
    for &player in players {
        scenario.add_real_card(player, "Leyline of Sanctity", Zone::Battlefield, db);
    }
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn lives(runner: &GameRunner) -> [i32; 3] {
    [runner.life(P0), runner.life(P1), runner.life(P2)]
}

fn graveyard_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].graveyard.len()
}

/// Announce casting `spell` with no pre-declared targets, so its target prompt
/// is raised and recorded by `drive` (CR 601.2c).
fn announce_cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce the spell");
}

fn opponents_legal(legal: &[TargetRef]) -> bool {
    legal.contains(&TargetRef::Player(P1))
        && legal.contains(&TargetRef::Player(P2))
        && !legal.contains(&TargetRef::Player(P0))
}

// ── Bane, Lord of Darkness ─────────────────────────────────────────────────

struct BaneBoard {
    runner: GameRunner,
    bears: ObjectId,
    bolt: ObjectId,
    c0: ObjectId,
    c0b: ObjectId,
    c1: ObjectId,
    c2: ObjectId,
}

/// Bane, Grizzly Bears and a Mountain (Lightning Bolt's printed {R}) under
/// P0; P0 holds the printed bolt, two 2/2 creature cards (C0, C0b, so the
/// put raises a choice) and a land; P1 and P2 each hold a 2/2 creature card
/// (C1, C2), so a hand-owner misreading shows up in the zone-choice
/// candidates themselves.
fn bane_board(db: &CardDatabase, leylines: &[PlayerId]) -> BaneBoard {
    let mut scenario = three_players();
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_real_card(P0, "Bane, Lord of Darkness", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let c0 = scenario
        .add_creature_to_hand(P0, "P0 Creature Card", 2, 2)
        .id();
    let c0b = scenario
        .add_creature_to_hand(P0, "P0 Second Creature Card", 2, 2)
        .id();
    scenario.add_land_to_hand(P0, "Forest");
    let c1 = scenario
        .add_creature_to_hand(P1, "P1 Creature Card", 2, 2)
        .id();
    let c2 = scenario
        .add_creature_to_hand(P2, "P2 Creature Card", 2, 2)
        .id();
    add_leylines(&mut scenario, leylines, db);
    BaneBoard {
        runner: scenario.build(),
        bears,
        bolt,
        c0,
        c0b,
        c1,
        c2,
    }
}

/// P0 bolts its own Grizzly Bears (a real death), then the trigger is driven.
fn bane_kill_bears(board: &mut BaneBoard, mut answers: Answers) -> Record {
    answers.targets.push_front(TargetRef::Object(board.bears));
    announce_cast(&mut board.runner, board.bolt);
    drive(&mut board.runner, answers)
}

/// T-Bane-d — CR 118.12 + CR 109.5 + CR 400.1: P2 declines, so the
/// CONTROLLER may put a creature card from the CONTROLLER's hand; P0 is the
/// one asked, and only P0's creature card is a candidate.
#[test]
fn bane_decline_controller_puts_from_own_hand() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut board = bane_board(db, &[]);
    let hands_before = [
        hand_len(&board.runner, P0),
        hand_len(&board.runner, P1),
        hand_len(&board.runner, P2),
    ];
    let mut answers = Answers::new(&[TargetRef::Player(P2)], &[false, true]);
    answers.zone_picks.push_back(vec![board.c0]);
    let record = bane_kill_bears(&mut board, answers);
    let runner = &board.runner;

    // REACH: the bolt's prompt, then the trigger's target prompt over the
    // opponents only (CR 115.1).
    assert_eq!(record.target_prompts.len(), 2, "{record:?}");
    assert!(opponents_legal(&record.target_prompts[1]), "{record:?}");
    // CR 608.2d: P2 decides the draw; P0 decides its own "you may put".
    assert_eq!(record.optional_actors, vec![P2, P0]);
    // CR 109.5 + CR 400.1: "your hand" is the controller's hand.
    assert_zone_choice_is_p0_hand(&record, &board);
    assert_eq!(runner.state().objects[&board.c0].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&board.c0].controller, P0);
    // The bolt and C0 left P0's hand; nobody drew.
    assert_eq!(hand_len(runner, P0), hands_before[0] - 2);
    assert_eq!(hand_len(runner, P1), hands_before[1]);
    assert_eq!(hand_len(runner, P2), hands_before[2]);
    assert_eq!(runner.state().objects[&board.c1].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&board.c2].zone, Zone::Hand);
}

/// T-Bane-a — CR 118.12: P2 accepts, so P0 draws one card and is not asked
/// to put anything.
#[test]
fn bane_accept_controller_draws() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut board = bane_board(db, &[]);
    let p0_hand = hand_len(&board.runner, P0);
    let record = bane_kill_bears(&mut board, Answers::new(&[TargetRef::Player(P2)], &[true]));
    assert_eq!(record.optional_actors, vec![P2]);
    assert!(record.zone_choices.is_empty(), "{record:?}");
    // The bolt left, one card was drawn.
    assert_eq!(hand_len(&board.runner, P0), p0_hand);
    assert_eq!(board.runner.state().objects[&board.c0].zone, Zone::Hand);
}

/// L-Bane — CR 603.3d: with every opponent hexproof the trigger has no legal
/// target and is removed; nobody is asked.
#[test]
fn bane_no_legal_target_removes_the_trigger() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut board = bane_board(db, &[P1, P2]);
    let p0_hand = hand_len(&board.runner, P0);
    let record = bane_kill_bears(&mut board, Answers::default());
    assert_eq!(record.target_prompts.len(), 1, "only the bolt: {record:?}");
    assert!(record.optional_actors.is_empty(), "{record:?}");
    assert!(record.zone_choices.is_empty(), "{record:?}");
    assert_eq!(hand_len(&board.runner, P0), p0_hand - 1);
}

/// L-Bane paired positive — CR 115.1: with only P1 hexproof, P2 is the sole
/// legal target and is chosen without a prompt; the decline result lands on
/// the controller's own hand.
#[test]
fn bane_sole_legal_target_is_chosen_without_prompt() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut board = bane_board(db, &[P1]);
    let mut answers = Answers::new(&[], &[false, true]);
    answers.zone_picks.push_back(vec![board.c0]);
    let record = bane_kill_bears(&mut board, answers);
    assert_eq!(record.target_prompts.len(), 1, "only the bolt: {record:?}");
    assert_eq!(record.optional_actors, vec![P2, P0]);
    assert_zone_choice_is_p0_hand(&record, &board);
    assert_eq!(
        board.runner.state().objects[&board.c0].zone,
        Zone::Battlefield
    );
}

/// CR 109.5 + CR 400.1: exactly one zone choice, asked of P0, whose
/// candidates are exactly P0's two creature cards.
fn assert_zone_choice_is_p0_hand(record: &Record, board: &BaneBoard) {
    let [(player, cards)] = record.zone_choices.as_slice() else {
        panic!("expected exactly one zone choice: {record:?}");
    };
    assert_eq!(*player, P0);
    let mut cards = cards.clone();
    cards.sort();
    let mut expected = vec![board.c0, board.c0b];
    expected.sort();
    assert_eq!(
        cards, expected,
        "only the controller's hand is a candidate pool"
    );
}

// ── Combustible Gearhulk ───────────────────────────────────────────────────

fn gearhulk_board(db: &CardDatabase, leylines: &[PlayerId]) -> (GameRunner, ObjectId) {
    let mut scenario = three_players();
    let gearhulk = scenario.add_real_card(P0, "Combustible Gearhulk", Zone::Hand, db);
    // Printed cost {4}{R}{R}: six Mountains pay it.
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    // `add_spell_to_library_top` inserts at index zero, so the reversed
    // filler order leaves the library top reading 2, 5, 1, 9.
    for (index, mana_value) in [2, 5, 1, 9].into_iter().rev().enumerate() {
        scenario
            .add_spell_to_library_top(P0, &format!("Gearhulk Library {index}"), false)
            .with_mana_cost(ManaCost::generic(mana_value));
    }
    add_leylines(&mut scenario, leylines, db);
    (scenario.build(), gearhulk)
}

/// T-Gear-d — CR 118.12 + CR 701.17a + CR 120.3a: P2 declines; P0 mills
/// three, and Gearhulk deals P2 (only P2) the milled cards' total mana value.
#[test]
fn gearhulk_decline_damages_only_the_target() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, gearhulk) = gearhulk_board(db, &[]);
    let before = lives(&runner);
    runner.cast(gearhulk).commit();
    let record = drive(
        &mut runner,
        Answers::new(&[TargetRef::Player(P2)], &[false]),
    );
    assert_eq!(record.target_prompts.len(), 1, "{record:?}");
    assert!(opponents_legal(&record.target_prompts[0]), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(graveyard_len(&runner, P0), 3);
    assert_eq!(lives(&runner), [before[0], before[1], before[2] - 8]);
}

/// T-Gear-a — CR 118.12: P2 accepts; P0 draws three and nothing is milled.
#[test]
fn gearhulk_accept_controller_draws_three() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, gearhulk) = gearhulk_board(db, &[]);
    let before = lives(&runner);
    let p0_hand = hand_len(&runner, P0);
    runner.cast(gearhulk).commit();
    let record = drive(&mut runner, Answers::new(&[TargetRef::Player(P2)], &[true]));
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(hand_len(&runner, P0), p0_hand - 1 + 3);
    assert_eq!(graveyard_len(&runner, P0), 0);
    assert_eq!(lives(&runner), before);
}

/// L-Gear — CR 603.3d: no legal target, so the trigger is removed.
#[test]
fn gearhulk_no_legal_target_removes_the_trigger() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, gearhulk) = gearhulk_board(db, &[P1, P2]);
    let before = lives(&runner);
    let p0_hand = hand_len(&runner, P0);
    runner.cast(gearhulk).commit();
    let record = drive(&mut runner, Answers::default());
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert!(record.optional_actors.is_empty(), "{record:?}");
    assert_eq!(hand_len(&runner, P0), p0_hand - 1);
    assert_eq!(graveyard_len(&runner, P0), 0);
    assert_eq!(lives(&runner), before);
}

/// L-Gear paired positive: P2 is the sole legal target, chosen without a
/// prompt, and takes the decline damage.
#[test]
fn gearhulk_sole_legal_target_is_chosen_without_prompt() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, gearhulk) = gearhulk_board(db, &[P1]);
    let before = lives(&runner);
    runner.cast(gearhulk).commit();
    let record = drive(&mut runner, Answers::new(&[], &[false]));
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(lives(&runner), [before[0], before[1], before[2] - 8]);
}

// ── Palantír of Orthanc ────────────────────────────────────────────────────

fn palantir_board(db: &CardDatabase, leylines: &[PlayerId]) -> (GameRunner, ObjectId) {
    let mut scenario = three_players();
    scenario.at_phase(Phase::PostCombatMain);
    let palantir = scenario.add_real_card(P0, "Palantír of Orthanc", Zone::Battlefield, db);
    scenario
        .add_spell_to_library_top(P0, "Palantír Library Top", false)
        .with_mana_cost(ManaCost::generic(3));
    add_leylines(&mut scenario, leylines, db);
    (scenario.build(), palantir)
}

fn influence(runner: &GameRunner, palantir: ObjectId) -> u32 {
    runner.state().objects[&palantir]
        .counters
        .get(&CounterType::Generic("influence".to_string()))
        .copied()
        .unwrap_or(0)
}

/// T-Pal-d — CR 118.12: P0 gets the counter and scries 2; P2 declines; P0
/// mills X = 1 and P2 (only P2) loses that card's mana value.
#[test]
fn palantir_decline_only_the_target_loses_life() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, palantir) = palantir_board(db, &[]);
    let before = lives(&runner);
    runner.advance_to_end_step();
    let record = drive(
        &mut runner,
        Answers::new(&[TargetRef::Player(P2)], &[false]),
    );
    assert_eq!(record.target_prompts.len(), 1, "{record:?}");
    assert!(opponents_legal(&record.target_prompts[0]), "{record:?}");
    assert_eq!(influence(&runner, palantir), 1);
    assert_eq!(record.scry_actors, vec![P0]);
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(graveyard_len(&runner, P0), 1);
    assert_eq!(lives(&runner), [before[0], before[1], before[2] - 3]);
}

/// T-Pal-a — CR 118.12: P2 accepts; P0 draws one, nothing is milled.
#[test]
fn palantir_accept_controller_draws() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, palantir) = palantir_board(db, &[]);
    let before = lives(&runner);
    let p0_hand = hand_len(&runner, P0);
    runner.advance_to_end_step();
    let record = drive(&mut runner, Answers::new(&[TargetRef::Player(P2)], &[true]));
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(influence(&runner, palantir), 1);
    assert_eq!(hand_len(&runner, P0), p0_hand + 1);
    assert_eq!(graveyard_len(&runner, P0), 0);
    assert_eq!(lives(&runner), before);
}

/// L-Pal — CR 603.3d: the whole ability is removed, so there is no counter
/// and no scry either.
#[test]
fn palantir_no_legal_target_removes_the_whole_ability() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, palantir) = palantir_board(db, &[P1, P2]);
    let before = lives(&runner);
    runner.advance_to_end_step();
    let record = drive(&mut runner, Answers::default());
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert!(record.optional_actors.is_empty(), "{record:?}");
    assert!(record.scry_actors.is_empty(), "{record:?}");
    assert_eq!(influence(&runner, palantir), 0);
    assert_eq!(lives(&runner), before);
}

/// L-Pal paired positive.
#[test]
fn palantir_sole_legal_target_is_chosen_without_prompt() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, palantir) = palantir_board(db, &[P1]);
    let before = lives(&runner);
    runner.advance_to_end_step();
    let record = drive(&mut runner, Answers::new(&[], &[false]));
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(influence(&runner, palantir), 1);
    assert_eq!(lives(&runner), [before[0], before[1], before[2] - 3]);
}

// ── Terrapact Intimidator ──────────────────────────────────────────────────

fn terrapact_board(db: &CardDatabase, leylines: &[PlayerId]) -> (GameRunner, ObjectId) {
    let mut scenario = three_players();
    let terrapact = scenario.add_real_card(P0, "Terrapact Intimidator", Zone::Hand, db);
    // Printed cost {1}{R}: two Mountains pay it.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    add_leylines(&mut scenario, leylines, db);
    (scenario.build(), terrapact)
}

/// `(owner, controller)` of every Lander token on the battlefield.
fn landers(runner: &GameRunner) -> Vec<(PlayerId, PlayerId)> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Lander")
        .map(|object| (object.owner, object.controller))
        .collect()
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// T-Ter-a — CR 118.12 + CR 111.2: P2 accepts; the two Landers belong to
/// and are controlled by P0, who created them.
#[test]
fn terrapact_accept_controller_creates_and_owns_landers() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, terrapact) = terrapact_board(db, &[]);
    runner.cast(terrapact).commit();
    let record = drive(&mut runner, Answers::new(&[TargetRef::Player(P2)], &[true]));
    assert_eq!(record.target_prompts.len(), 1, "{record:?}");
    assert!(opponents_legal(&record.target_prompts[0]), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(landers(&runner), vec![(P0, P0), (P0, P0)]);
    assert_eq!(plus_counters(&runner, terrapact), 0);
}

/// T-Ter-d — CR 118.12: P2 declines; two +1/+1 counters, no Lander.
#[test]
fn terrapact_decline_gets_two_counters() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, terrapact) = terrapact_board(db, &[]);
    runner.cast(terrapact).commit();
    let record = drive(
        &mut runner,
        Answers::new(&[TargetRef::Player(P2)], &[false]),
    );
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(plus_counters(&runner, terrapact), 2);
    assert!(landers(&runner).is_empty());
}

/// L-Ter — CR 603.3d, with its paired positive.
#[test]
fn terrapact_no_legal_target_removes_the_trigger() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, terrapact) = terrapact_board(db, &[P1, P2]);
    runner.cast(terrapact).commit();
    let record = drive(&mut runner, Answers::default());
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert!(record.optional_actors.is_empty(), "{record:?}");
    assert_eq!(plus_counters(&runner, terrapact), 0);
    assert!(landers(&runner).is_empty());

    let (mut runner, terrapact) = terrapact_board(db, &[P1]);
    runner.cast(terrapact).commit();
    let record = drive(&mut runner, Answers::new(&[], &[false]));
    assert!(record.target_prompts.is_empty(), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(plus_counters(&runner, terrapact), 2);
    assert!(landers(&runner).is_empty());
}

// ── Risk Factor ────────────────────────────────────────────────────────────

fn risk_factor_board(db: &CardDatabase, leylines: &[PlayerId]) -> (GameRunner, ObjectId) {
    let mut scenario = three_players();
    let risk = scenario.add_real_card(P0, "Risk Factor", Zone::Hand, db);
    // Printed cost {2}{R}: three Mountains pay it.
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    add_leylines(&mut scenario, leylines, db);
    (scenario.build(), risk)
}

/// T-Risk-a — CR 115.1 + CR 118.12 + CR 120.3a: only opponents may be
/// targeted; P2 accepts and Risk Factor deals 4 damage to "them" = P2.
#[test]
fn risk_factor_accept_damages_only_the_target() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, risk) = risk_factor_board(db, &[]);
    let before = lives(&runner);
    let p0_hand = hand_len(&runner, P0);
    announce_cast(&mut runner, risk);
    let record = drive(&mut runner, Answers::new(&[TargetRef::Player(P2)], &[true]));
    assert_eq!(record.target_prompts.len(), 1, "{record:?}");
    assert!(opponents_legal(&record.target_prompts[0]), "{record:?}");
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(lives(&runner), [before[0], before[1], before[2] - 4]);
    assert_eq!(hand_len(&runner, P0), p0_hand - 1);
}

/// T-Risk-d — CR 118.12: P2 declines; P0 draws three and nobody is damaged.
#[test]
fn risk_factor_decline_controller_draws_three() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, risk) = risk_factor_board(db, &[]);
    let before = lives(&runner);
    let p0_hand = hand_len(&runner, P0);
    announce_cast(&mut runner, risk);
    let record = drive(
        &mut runner,
        Answers::new(&[TargetRef::Player(P2)], &[false]),
    );
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(lives(&runner), before);
    assert_eq!(hand_len(&runner, P0), p0_hand - 1 + 3);
}

/// L-Risk — CR 601.2c + CR 115.1a: with every opponent hexproof Risk Factor
/// has no legal target and cannot be cast; it stays in hand.
#[test]
fn risk_factor_without_legal_target_cannot_be_cast() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, risk) = risk_factor_board(db, &[P1, P2]);
    let before = lives(&runner);
    let card_id = runner.state().objects[&risk].card_id;
    let cast = runner.act(GameAction::CastSpell {
        object_id: risk,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    let still_in_hand = runner.state().objects[&risk].zone == Zone::Hand;
    assert!(
        cast.is_err() || still_in_hand,
        "CR 601.2c: no legal target, no cast ({cast:?})"
    );
    assert!(still_in_hand);
    assert!(runner.state().stack.is_empty());
    assert_eq!(lives(&runner), before);

    // Paired positive: only P1 hexproof, so the cast succeeds on P2.
    let (mut runner, risk) = risk_factor_board(db, &[P1]);
    let before = lives(&runner);
    announce_cast(&mut runner, risk);
    let record = drive(&mut runner, Answers::new(&[TargetRef::Player(P2)], &[true]));
    assert_eq!(record.optional_actors, vec![P2]);
    assert_eq!(lives(&runner), [before[0], before[1], before[2] - 4]);
}
