//! EH1: "a player cast two or more spells last turn" (the werewolf transform clause) and its
//! table-wide siblings, driven through the production cast, turn-structure and trigger pipeline
//! with real printed cards (verbatim Oracle text, asserted against the card database below).
//!
//! Cards (verbatim Oracle text, asserted against the card database below):
//! - Gatstaf Shepherd // Gatstaf Howler. Front: "At the beginning of each upkeep, if no spells were
//!   cast last turn, transform this creature." Back: "Intimidate ... / At the beginning of each
//!   upkeep, if a player cast two or more spells last turn, transform this creature."
//!   Ruling: "If multiple players each cast just one spell during the previous turn, the ability
//!   won't trigger."
//! - Call of the Full Moon: "Enchant creature / Enchanted creature gets +3/+2 and has trample. / At
//!   the beginning of each upkeep, if a player cast two or more spells last turn, sacrifice this
//!   Aura." Ruling: it "will look at the entire previous turn, even if Call of the Full Moon wasn't
//!   on the battlefield for some or all of that turn", and the Aura's own cast counts.
//! - Shock, Opt, Time Walk ("Take an extra turn after this one."), Meditate ("Draw four cards. You
//!   skip your next turn.") produce the casts and the extra and skipped turns.
//!
//! Rules: CR 601.2i (a spell becomes cast), CR 500.7 (extra turn), CR 614.10 and CR 500.11 (a skipped
//! turn never begins), CR 603.4 (intervening-if), CR 800.4i (a departed player's actions can still
//! be found), CR 502.2 / 731.2 (day/night reads the previous turn's ACTIVE player only).
//!
//! Every negative row is paired with a positive reach guard in the same fixture (the trigger fires
//! for the sibling distribution, or the zone/face changed), so no row passes vacuously. Each row
//! flips under the revert named in its doc comment (a snapshot never written, a table-wide
//! sum, an active-player-only latch, a skipped turn latched as a zero turn, and so on).

use engine::database::card_db::CardDatabase;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{DayNight, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P2: PlayerId = PlayerId(2);

const SHEPHERD: &str = "Gatstaf Shepherd";
const SHEPHERD_ORACLE: &str =
    "At the beginning of each upkeep, if no spells were cast last turn, transform this creature.";
const HOWLER: &str = "Gatstaf Howler";
const HOWLER_ORACLE: &str = "Intimidate (This creature can't be blocked except by artifact creatures and/or creatures that share a color with it.)\nAt the beginning of each upkeep, if a player cast two or more spells last turn, transform this creature.";
const CALL: &str = "Call of the Full Moon";
const CALL_ORACLE: &str = "Enchant creature\nEnchanted creature gets +3/+2 and has trample. (It can deal excess combat damage to the player or planeswalker it's attacking.)\nAt the beginning of each upkeep, if a player cast two or more spells last turn, sacrifice this Aura.";
const SHOCK: &str = "Shock";
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";
const OPT: &str = "Opt";
const OPT_ORACLE: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";
const TIME_WALK: &str = "Time Walk";
const TIME_WALK_ORACLE: &str = "Take an extra turn after this one.";
const MEDITATE: &str = "Meditate";
const MEDITATE_ORACLE: &str = "Draw four cards. You skip your next turn.";
const BEARS: &str = "Grizzly Bears";
const NATURALIST: &str = "Kessig Naturalist";
const NATURALIST_ORACLE: &str = "Whenever this creature attacks, add {R} or {G}. Until end of turn, you don't lose this mana as steps and phases end.\nDaybound (If a player casts no spells during their own turn, it becomes night next turn.)";
const LORD: &str = "Lord of the Ulvenwald";

fn assert_oracle(db: &CardDatabase, name: &str, oracle: &str) {
    assert_eq!(
        db.get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the card database"))
            .oracle_text
            .as_deref(),
        Some(oracle),
        "{name} must carry its verbatim printed Oracle text"
    );
}

/// Real cards in hand/battlefield for the staged board. Libraries are stocked so no draw step can
/// end the game (CR 704.5b).
struct Board {
    runner: GameRunner,
    /// The double-faced permanent under test (front: Gatstaf Shepherd).
    dfc: ObjectId,
}

/// Which face of the double-faced permanent starts face up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Face {
    Shepherd,
    Howler,
}

fn fund(runner: &mut GameRunner, player: PlayerId, mana: ManaType, n: usize) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    for _ in 0..n {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
}

fn settle(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn face_name(runner: &GameRunner, id: ObjectId) -> String {
    runner.state().objects[&id].name.clone()
}

/// Setup only: the staged permanent is made to show its back face without paying for it.
fn show_howler(runner: &mut GameRunner, dfc: ObjectId) {
    engine::game::transform::transform_permanent(runner.state_mut(), dfc, &mut Vec::new())
        .expect("the staged Gatstaf must transform");
    settle(runner);
    assert_eq!(face_name(runner, dfc), HOWLER);
}

/// `players` seats, the double-faced permanent on P0's battlefield showing `face`, `shocks` Shocks in
/// each player's hand, `extra` is called to stage further real cards.
fn board(
    players: u8,
    face: Face,
    shocks: usize,
    extra: impl FnOnce(&mut GameScenario, &'static CardDatabase),
) -> (Board, Vec<Vec<ObjectId>>) {
    let db = load_db().expect("card db");
    for (name, oracle) in [
        (SHEPHERD, SHEPHERD_ORACLE),
        (HOWLER, HOWLER_ORACLE),
        (SHOCK, SHOCK_ORACLE),
    ] {
        assert_oracle(db, name, oracle);
    }
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    let dfc = scenario.add_real_card(P0, SHEPHERD, Zone::Battlefield, db);
    let mut hands = Vec::new();
    for seat in 0..players {
        let player = PlayerId(seat);
        scenario.with_library_top(
            player,
            &[
                "Top 1", "Top 2", "Top 3", "Top 4", "Top 5", "Top 6", "Top 7", "Top 8",
            ],
        );
        hands.push(
            (0..shocks)
                .map(|_| scenario.add_real_card(player, SHOCK, Zone::Hand, db))
                .collect::<Vec<_>>(),
        );
    }
    extra(&mut scenario, db);
    let mut runner = scenario.build();
    if face == Face::Howler {
        show_howler(&mut runner, dfc);
    }
    for seat in 0..players {
        fund(&mut runner, PlayerId(seat), ManaType::Red, 6);
        fund(&mut runner, PlayerId(seat), ManaType::Blue, 6);
    }
    (Board { runner, dfc }, hands)
}

fn priority_holder(runner: &GameRunner) -> PlayerId {
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => player,
        ref other => panic!("expected a priority window, got {other:?}"),
    }
}

/// Passes priority one player at a time (never all in a row, so no step ends) until `who` holds it.
fn give_priority(runner: &mut GameRunner, who: PlayerId) {
    for _ in 0..runner.state().players.len() {
        if priority_holder(runner) == who {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    assert_eq!(
        priority_holder(runner),
        who,
        "could not hand priority to {who:?}"
    );
}

/// CR 601.2i: `who` casts `spell` at `target`'s face and it resolves; the spell must end in
/// the graveyard (a reach guard that the cast really completed).
fn cast_at_player(runner: &mut GameRunner, who: PlayerId, spell: ObjectId, target: PlayerId) {
    give_priority(runner, who);
    runner.cast(spell).target_player(target).resolve();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "the cast of {spell:?} completed"
    );
}

/// Casts a spell with no target.
fn cast_untargeted(runner: &mut GameRunner, who: PlayerId, spell: ObjectId) {
    give_priority(runner, who);
    runner.cast(spell).resolve();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

/// What an upkeep looked like when the step began.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Upkeep {
    turn: u32,
    active: PlayerId,
    /// Triggered abilities on the stack when the upkeep step opened.
    triggers: usize,
}

/// One step toward the next upkeep: passes priority, or declares no attackers when the turn
/// structure stops at the declare-attackers prompt (a creature of the active player is able to
/// attack; nobody does in these fixtures).
fn advance_one(runner: &mut GameRunner) {
    match runner.state().waiting_for {
        WaitingFor::DeclareAttackers { .. } => {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
        }
        _ => {
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
    }
}

/// Passes priority until the next upkeep step begins (a later turn than the one running now) and
/// returns what the step opened with, leaving its triggers on the stack.
fn open_next_upkeep(runner: &mut GameRunner) -> Upkeep {
    let start = runner.state().turn_number;
    for _ in 0..200 {
        if runner.state().phase == Phase::Upkeep && runner.state().turn_number > start {
            return Upkeep {
                turn: runner.state().turn_number,
                active: runner.state().active_player,
                triggers: runner.state().stack.len(),
            };
        }
        advance_one(runner);
    }
    panic!("no further upkeep step began");
}

/// Resolves everything on the stack (CR 603.4: an intervening-if is rechecked as each ability
/// resolves).
fn resolve_stack(runner: &mut GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the stack");
        guard += 1;
        assert!(guard < 16, "the stack never emptied");
    }
}

/// [`open_next_upkeep`], then [`resolve_stack`].
fn next_upkeep(runner: &mut GameRunner) -> Upkeep {
    let seen = open_next_upkeep(runner);
    resolve_stack(runner);
    seen
}

/// Moves a stepping game from the upkeep it is in to the active player's precombat main phase and
/// funds both players again (mana empties at every step end).
fn to_main(runner: &mut GameRunner, players: u8) {
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    for seat in 0..players {
        fund(runner, PlayerId(seat), ManaType::Red, 6);
        fund(runner, PlayerId(seat), ManaType::Blue, 6);
    }
}

/// Plays turn N: P0 casts `p0` Shocks, then every other player casts `others[i]` Shocks (all at the
/// opponent's face); returns the upkeep that follows (turn N+1).
fn turn_with_casts(b: &mut Board, hands: &[Vec<ObjectId>], counts: &[usize]) -> Upkeep {
    for (seat, count) in counts.iter().enumerate() {
        let player = PlayerId(seat as u8);
        let target = if seat == 0 { P1 } else { P0 };
        for &spell in hands[seat].iter().take(*count) {
            cast_at_player(&mut b.runner, player, spell, target);
        }
    }
    next_upkeep(&mut b.runner)
}

fn howler_row(counts: &[usize], players: u8) -> (Upkeep, String) {
    let (mut b, hands) = board(players, Face::Howler, 3, |_, _| {});
    let upkeep = turn_with_casts(&mut b, &hands, counts);
    let face = face_name(&b.runner, b.dfc);
    (upkeep, face)
}

fn shepherd_row(counts: &[usize]) -> (Upkeep, String) {
    let (mut b, hands) = board(2, Face::Shepherd, 3, |_, _| {});
    let upkeep = turn_with_casts(&mut b, &hands, counts);
    let face = face_name(&b.runner, b.dfc);
    (upkeep, face)
}

// ===== W1: one player casts two ========================================================

/// W1. CR 601.2i + CR 603.4: Howler on P0's battlefield, P0 casts two spells in turn N and P1 none:
/// at the next upkeep Howler's intervening-if holds, one trigger resolves and the permanent shows
/// its front face (Shepherd). The (0,0) and (1,0) rows are the paired negatives.
#[test]
fn w1_one_player_casting_two_spells_transforms_howler() {
    let (upkeep, face) = howler_row(&[2, 0], 2);
    assert_eq!(
        upkeep.triggers, 1,
        "the Howler trigger was put on the stack"
    );
    assert_eq!(face, SHEPHERD, "reach: the transform trigger resolved");

    // Paired negatives: the same fixture with fewer casts leaves Howler untouched.
    for counts in [[1, 0], [0, 0]] {
        let (upkeep, face) = howler_row(&counts, 2);
        assert_eq!(upkeep.triggers, 0, "{counts:?}: no trigger");
        assert_eq!(face, HOWLER, "{counts:?}: face unchanged");
    }
}

// ===== W2: two players cast one each (the #10004 misread) ===============================

/// W2. Ruling: "If multiple players each cast just one spell during the previous turn, the ability
/// won't trigger." The reach guard is the same fixture's (2,0) positive and the per-player records.
#[test]
fn w2_one_spell_each_does_not_trigger_howler() {
    let (mut b, hands) = board(2, Face::Howler, 3, |_, _| {});
    let upkeep = turn_with_casts(&mut b, &hands, &[1, 1]);
    // Reach: both players really cast exactly one spell this boundary latched.
    let latched: Vec<u32> = b
        .runner
        .state()
        .players
        .iter()
        .map(|p| p.spells_cast_last_turn)
        .collect();
    assert_eq!(latched, vec![1, 1]);
    assert_eq!(
        upkeep.triggers, 0,
        "1 + 1 is not 'a player cast two or more'"
    );
    assert_eq!(face_name(&b.runner, b.dfc), HOWLER);

    // Positive reach in the same shape: (2,0) fires.
    let (upkeep, face) = howler_row(&[2, 0], 2);
    assert_eq!((upkeep.triggers, face.as_str()), (1, SHEPHERD));
}

// ===== W3: the non-active player cast two ===============================================

/// W3. CR 502.2 does not apply: Howler counts the NON-active player's casts too. (0,2) and (1,2)
/// fire; the active-player-only reader (`most_spells_cast_by_one_previous_active_player`) would
/// miss both.
#[test]
fn w3_non_active_player_casting_two_transforms_howler() {
    for counts in [[0, 2], [1, 2]] {
        let (upkeep, face) = howler_row(&counts, 2);
        assert_eq!(upkeep.triggers, 1, "{counts:?}: trigger");
        assert_eq!(face, SHEPHERD, "{counts:?}: transformed");
    }
}

/// W3 (day/night). CR 731.2 + CR 502.2: the day/night designation reads only the previous turn's
/// ACTIVE player's casts, while Howler counts every player's. A Kessig Naturalist (daybound) sits
/// beside Howler; each row asserts Howler's transform (the paired positive) AND the designation:
/// from day, (1,2): day stays day (P0 cast one) and Howler fires on P1's two; (0,2): it becomes
/// night (P0 cast none, CR 731.2a), the Naturalist shows its back face, and Howler still fires;
/// from night, (2,0): it becomes day (CR 731.2b), the Naturalist shows its front; (1,2): it stays
/// night (only the active player counts) while Howler fires. The "established" designation is a
/// direct precondition (the printed Naturalist is on the battlefield; no cast establishes it
/// here), recorded as the harness fallback.
fn day_night_row(start: DayNight, counts: &[usize]) -> (Upkeep, String, Option<DayNight>, String) {
    let mut staged = None;
    let (mut b, hands) = board(2, Face::Howler, 3, |scenario, db| {
        assert_oracle(db, NATURALIST, NATURALIST_ORACLE);
        staged = Some(scenario.add_real_card(P0, NATURALIST, Zone::Battlefield, db));
    });
    let naturalist = staged.expect("staged");
    b.runner.state_mut().day_night = Some(start);
    if start == DayNight::Night {
        // CR 702.145b/e: a daybound permanent only turns over through its own designation.
        engine::game::transform::transform_permanent_with_cause(
            b.runner.state_mut(),
            naturalist,
            engine::game::transform::TransformCause::DayNightDesignation,
            &mut Vec::new(),
        )
        .expect("the daybound permanent shows its nightbound face at night");
    }
    settle(&mut b.runner);
    assert_eq!(
        face_name(&b.runner, naturalist),
        if start == DayNight::Night {
            LORD
        } else {
            NATURALIST
        },
        "reach: the Naturalist shows the face matching the designation"
    );
    let upkeep = turn_with_casts(&mut b, &hands, counts);
    (
        upkeep,
        face_name(&b.runner, b.dfc),
        b.runner.state().day_night,
        face_name(&b.runner, naturalist),
    )
}

#[test]
fn w3_day_night_reads_the_active_player_while_howler_reads_every_player() {
    // From day.
    let (up, howler, designation, naturalist) = day_night_row(DayNight::Day, &[1, 2]);
    assert_eq!(
        (
            up.triggers,
            howler.as_str(),
            designation,
            naturalist.as_str()
        ),
        (1, SHEPHERD, Some(DayNight::Day), NATURALIST),
        "day, (1,2)"
    );
    let (up, howler, designation, naturalist) = day_night_row(DayNight::Day, &[0, 2]);
    assert_eq!(
        (
            up.triggers,
            howler.as_str(),
            designation,
            naturalist.as_str()
        ),
        (1, SHEPHERD, Some(DayNight::Night), LORD),
        "day, (0,2): the active player cast none"
    );
    // From night.
    let (up, howler, designation, naturalist) = day_night_row(DayNight::Night, &[2, 0]);
    assert_eq!(
        (
            up.triggers,
            howler.as_str(),
            designation,
            naturalist.as_str()
        ),
        (1, SHEPHERD, Some(DayNight::Day), NATURALIST),
        "night, (2,0): the active player cast two"
    );
    let (up, howler, designation, naturalist) = day_night_row(DayNight::Night, &[1, 2]);
    assert_eq!(
        (
            up.triggers,
            howler.as_str(),
            designation,
            naturalist.as_str()
        ),
        (1, SHEPHERD, Some(DayNight::Night), LORD),
        "night, (1,2): only the active player counts for the designation"
    );
}

// ===== W4: casts before the permanent arrived ===========================================

/// W4. Ruling: "even if the Werewolf ... wasn't on the battlefield for some or all of that turn".
/// P0 casts two spells while Howler is still in hand, then it enters; it triggers at the next
/// upkeep.
#[test]
fn w4_casts_before_the_permanent_arrived_still_count() {
    let (mut b, hands) = board(2, Face::Shepherd, 3, |_, _| {});
    // Replace the staged permanent with one that is in P0's hand during the casts.
    let hand_howler = b.dfc;
    move_to_zone(
        b.runner.state_mut(),
        hand_howler,
        Zone::Hand,
        &mut Vec::new(),
    );
    assert_eq!(b.runner.state().objects[&hand_howler].zone, Zone::Hand);
    for &spell in hands[0].iter().take(2) {
        assert_ne!(
            b.runner.state().objects[&hand_howler].zone,
            Zone::Battlefield,
            "absent from the battlefield while the spells are cast"
        );
        cast_at_player(&mut b.runner, P0, spell, P1);
    }
    move_to_zone(
        b.runner.state_mut(),
        hand_howler,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    settle(&mut b.runner);
    show_howler(&mut b.runner, hand_howler);
    let upkeep = next_upkeep(&mut b.runner);
    assert_eq!(upkeep.triggers, 1);
    assert_eq!(face_name(&b.runner, hand_howler), SHEPHERD);
}

// ===== W5: Call of the Full Moon ========================================================

fn call_board() -> (Board, Vec<Vec<ObjectId>>, ObjectId, ObjectId) {
    let mut staged = None;
    let (b, hands) = board(2, Face::Shepherd, 2, |scenario, db| {
        assert_oracle(db, CALL, CALL_ORACLE);
        let bears = scenario.add_real_card(P0, BEARS, Zone::Battlefield, db);
        let call = scenario.add_real_card(P0, CALL, Zone::Hand, db);
        staged = Some((bears, call));
    });
    let (bears, call) = staged.expect("staged");
    (b, hands, bears, call)
}

fn pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let o = &runner.state().objects[&id];
    (o.power, o.toughness)
}

/// W5(a). The Aura's own cast is one of the two spells (the ruling's example): P0 casts Call of the
/// Full Moon (1), it resolves attached, then P0 casts Shock (2): at the next upkeep the Aura is
/// sacrificed and the creature loses +3/+2.
#[test]
fn w5a_call_of_the_full_moon_is_sacrificed_after_two_casts() {
    let (mut b, hands, bears, call) = call_board();
    // The staged Shepherd is not the subject here; keep it from interfering.
    give_priority(&mut b.runner, P0);
    b.runner.cast(call).target_object(bears).resolve();
    settle(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&call].zone,
        Zone::Battlefield,
        "reach: the Aura resolved"
    );
    assert_eq!(
        pt(&b.runner, bears),
        (Some(5), Some(4)),
        "reach: +3/+2 applied"
    );
    cast_at_player(&mut b.runner, P0, hands[0][0], P1);
    let upkeep = next_upkeep(&mut b.runner);
    assert_eq!(
        upkeep.triggers, 1,
        "the Aura's trigger (the Shepherd's guard is false)"
    );
    assert_eq!(
        b.runner.state().objects[&call].zone,
        Zone::Graveyard,
        "Aura sacrificed"
    );
    settle(&mut b.runner);
    assert_eq!(
        pt(&b.runner, bears),
        (Some(2), Some(2)),
        "the creature lost +3/+2"
    );
}

/// W5(b). Reverse-fail: the Aura's cast (1) and P1's Shock (1) are two spells but not two by ONE
/// player, so the Aura survives, still attached.
#[test]
fn w5b_call_of_the_full_moon_survives_one_cast_each() {
    let (mut b, hands, bears, call) = call_board();
    give_priority(&mut b.runner, P0);
    b.runner.cast(call).target_object(bears).resolve();
    settle(&mut b.runner);
    assert_eq!(b.runner.state().objects[&call].zone, Zone::Battlefield);
    cast_at_player(&mut b.runner, P1, hands[1][0], P0);
    let upkeep = next_upkeep(&mut b.runner);
    // Reach: each player's own count was latched (the Aura's cast and P1's Shock), and the same
    // board with P0 casting the second spell (W5a) does sacrifice.
    let latched: Vec<u32> = b
        .runner
        .state()
        .players
        .iter()
        .map(|p| p.spells_cast_last_turn)
        .collect();
    assert_eq!(latched, vec![1, 1]);
    assert_eq!(
        upkeep.triggers, 0,
        "no upkeep trigger: nobody cast two, and spells were cast"
    );
    assert_eq!(
        b.runner.state().objects[&call].zone,
        Zone::Battlefield,
        "Aura survives"
    );
    settle(&mut b.runner);
    assert_eq!(pt(&b.runner, bears), (Some(5), Some(4)));
}

/// W5(c). The Aura cast alone (1,0): it survives.
#[test]
fn w5c_call_of_the_full_moon_survives_a_single_cast() {
    let (mut b, _hands, bears, call) = call_board();
    give_priority(&mut b.runner, P0);
    b.runner.cast(call).target_object(bears).resolve();
    settle(&mut b.runner);
    assert_eq!(b.runner.state().objects[&call].zone, Zone::Battlefield);
    let upkeep = next_upkeep(&mut b.runner);
    let latched: Vec<u32> = b
        .runner
        .state()
        .players
        .iter()
        .map(|p| p.spells_cast_last_turn)
        .collect();
    assert_eq!(latched, vec![1, 0], "reach: the Aura's cast was latched");
    assert_eq!(upkeep.triggers, 0);
    assert_eq!(b.runner.state().objects[&call].zone, Zone::Battlefield);
}

// ===== W6: two-turn sequence ============================================================

/// W6. Howler transforms at N+1's upkeep; Shepherd's own trigger transforms it back at N+2's (no
/// casts in N+1); at N+3 (no casts in N+2) Howler's condition is false. A stale per-player snapshot
/// leaves Shepherd at N+2; one that never clears transforms again at N+3.
#[test]
fn w6_the_snapshot_is_overwritten_at_every_boundary() {
    let (mut b, hands) = board(2, Face::Howler, 3, |_, _| {});
    let n1 = turn_with_casts(&mut b, &hands, &[2, 0]);
    assert_eq!(
        (n1.triggers, face_name(&b.runner, b.dfc)),
        (1, SHEPHERD.to_string())
    );
    // Nobody casts in N+1.
    let n2 = next_upkeep(&mut b.runner);
    assert_eq!(
        (n2.triggers, face_name(&b.runner, b.dfc)),
        (1, HOWLER.to_string())
    );
    // Nobody casts in N+2 either: Howler's condition is false at N+3.
    let n3 = next_upkeep(&mut b.runner);
    assert_eq!(
        (n3.triggers, face_name(&b.runner, b.dfc)),
        (0, HOWLER.to_string())
    );
}

// ===== W7: Shepherd's table-wide guard ==================================================

/// W7. Gatstaf Shepherd ("no spells were cast last turn") keeps its table-wide meaning: only (0,0)
/// fires; every row with a cast does not.
#[test]
fn w7_shepherd_transforms_only_when_nobody_cast() {
    let (upkeep, face) = shepherd_row(&[0, 0]);
    assert_eq!((upkeep.triggers, face.as_str()), (1, HOWLER), "(0,0) fires");
    for counts in [[1, 0], [0, 1], [1, 1], [2, 0]] {
        let (upkeep, face) = shepherd_row(&counts);
        assert_eq!(upkeep.triggers, 0, "{counts:?}");
        assert_eq!(face, SHEPHERD, "{counts:?}");
    }
}

// ===== W8: extra turn ===================================================================

/// Faces and trigger counts at the two upkeeps after turn N, with `producer` the second spell
/// P0 casts in N next to a Shock: Time Walk (an extra turn E follows) or Opt (the ordinary
/// control). Nobody casts after N.
fn extra_turn_or_control(extra_turn: bool) -> [(Upkeep, String); 2] {
    let mut staged = None;
    let (mut b, hands) = board(2, Face::Howler, 3, |scenario, db| {
        assert_oracle(db, TIME_WALK, TIME_WALK_ORACLE);
        assert_oracle(db, OPT, OPT_ORACLE);
        staged = Some((
            scenario.add_real_card(P0, TIME_WALK, Zone::Hand, db),
            scenario.add_real_card(P0, OPT, Zone::Hand, db),
        ));
    });
    let (time_walk, opt) = staged.expect("staged");
    cast_untargeted(&mut b.runner, P0, if extra_turn { time_walk } else { opt });
    cast_at_player(&mut b.runner, P0, hands[0][0], P1);
    let first = next_upkeep(&mut b.runner);
    let first_face = face_name(&b.runner, b.dfc);
    let second = next_upkeep(&mut b.runner);
    let second_face = face_name(&b.runner, b.dfc);
    [(first, first_face), (second, second_face)]
}

/// W8. CR 500.7: Time Walk gives P0 an extra turn E after turn N. Turn N: Time Walk (1) and Shock
/// (2). At E's upkeep (last turn = N) Howler fires and shows Shepherd; nobody casts in E; at the
/// next upkeep (P1's, last turn = E) Shepherd fires and shows Howler. A snapshot taken only at an
/// ordinary turn change reads turn N-1 at E. W8c is the ordinary-turn control (Opt in place of
/// Time Walk): the face and trigger-count sequence must be identical.
#[test]
fn w8_extra_turn_is_a_turn_for_last_turn() {
    let [(e, e_face), (after, after_face)] = extra_turn_or_control(true);
    assert_eq!(e.active, P0, "reach: the extra turn is P0's");
    assert_eq!((e.triggers, e_face.as_str()), (1, SHEPHERD));
    assert_eq!(after.active, P1, "the turn after the extra turn is P1's");
    assert_eq!((after.triggers, after_face.as_str()), (1, HOWLER));
}

#[test]
fn w8c_ordinary_turn_control_matches_the_extra_turn_sequence() {
    let [(n1, n1_face), (n2, n2_face)] = extra_turn_or_control(false);
    assert_eq!(n1.active, P1, "reach: no extra turn, P1 follows");
    assert_eq!((n1.triggers, n1_face.as_str()), (1, SHEPHERD));
    assert_eq!(n2.active, P0);
    assert_eq!((n2.triggers, n2_face.as_str()), (1, HOWLER));
    // The same trigger-count and face sequence as the extra-turn fixture.
    let [(e, e_face), (after, after_face)] = extra_turn_or_control(true);
    assert_eq!(
        [(e.triggers, e_face), (after.triggers, after_face)],
        [(n1.triggers, n1_face), (n2.triggers, n2_face)]
    );
}

// ===== W9: skipped turn =================================================================

/// W9 family. CR 614.10 + CR 500.11: Meditate makes P0 skip a turn; Opt is the ordinary-turn
/// control (Opt replaces Meditate, so every fixture has P0 cast two spells in turn N). In N+1
/// P1 casts Shock and Opt (`p1_casts`) or nothing. Returns the upkeeps (with the face after
/// they resolve) of N+1 and of the next turn actually taken.
fn skip_or_control(meditate: bool, p1_casts: bool) -> [(Upkeep, String); 2] {
    let mut staged = None;
    let (mut b, hands) = board(2, Face::Howler, 3, |scenario, db| {
        for (name, oracle) in [(MEDITATE, MEDITATE_ORACLE), (OPT, OPT_ORACLE)] {
            assert_oracle(db, name, oracle);
        }
        staged = Some((
            scenario.add_real_card(P0, MEDITATE, Zone::Hand, db),
            scenario.add_real_card(P0, OPT, Zone::Hand, db),
            scenario.add_real_card(P1, OPT, Zone::Hand, db),
        ));
    });
    let (meditate_card, opt0, opt1) = staged.expect("staged");
    // Turn N: P0 casts the producer (1), then Shock (2).
    cast_untargeted(
        &mut b.runner,
        P0,
        if meditate { meditate_card } else { opt0 },
    );
    cast_at_player(&mut b.runner, P0, hands[0][0], P1);
    // N+1 (P1's turn): upkeep, then P1 casts Shock and Opt (2) or nothing.
    let n1 = next_upkeep(&mut b.runner);
    let n1_face = face_name(&b.runner, b.dfc);
    if p1_casts {
        to_main(&mut b.runner, 2);
        cast_at_player(&mut b.runner, P1, hands[1][0], P0);
        cast_untargeted(&mut b.runner, P1, opt1);
    }
    let next = next_upkeep(&mut b.runner);
    let next_face = face_name(&b.runner, b.dfc);
    [(n1, n1_face), (next, next_face)]
}

#[test]
fn w9_a_skipped_turn_is_never_latched_as_a_zero_turn() {
    let [(n1, n1_face), (next, next_face)] = skip_or_control(true, true);
    // N+1: Howler fires (P0 cast two) and shows Shepherd.
    assert_eq!(
        (n1.active, n1.triggers, n1_face.as_str()),
        (P1, 1, SHEPHERD)
    );
    // P0's turn N+2 is skipped: no P0 upkeep begins, the next upkeep is P1's again, and the
    // skipped turn consumed a turn number (the restart counts it). Shepherd's condition ("no
    // spells were cast last turn") is false because P1 cast two in N+1. A zero-turn latch for the
    // skipped turn would make it true and transform the permanent back.
    assert_eq!(next.active, P1, "reach: P0's upkeep never began");
    assert_eq!(
        next.turn,
        n1.turn + 2,
        "reach: the skipped turn consumed a turn number"
    );
    assert_eq!((next.triggers, next_face.as_str()), (0, SHEPHERD));

    // Ordinary-turn control: the same fixture with Opt in place of Meditate.
    let [(c1, c1_face), (control, control_face)] = skip_or_control(false, true);
    assert_eq!(
        (c1.active, c1.triggers, c1_face.as_str()),
        (P1, 1, SHEPHERD)
    );
    assert_eq!(control.active, P0, "P0's upkeep occurs and agrees");
    assert_eq!(control.turn, c1.turn + 1);
    assert_eq!((control.triggers, control_face.as_str()), (0, SHEPHERD));
}

/// W9d / W9e. P1 casts nothing in N+1: at the next taken upkeep (last turn = N+1, zero casts)
/// Shepherd's guard holds and the face returns to Howler, skip and control alike (the sequences
/// are equal). A snapshot not overwritten at the N+1 boundary still reads N (P0 cast two) and
/// leaves Shepherd there.
#[test]
fn w9d_w9e_a_quiet_turn_after_the_producer_returns_howler_skip_or_not() {
    let [(n1, n1_face), (next, next_face)] = skip_or_control(true, false);
    assert_eq!((n1.triggers, n1_face.as_str()), (1, SHEPHERD));
    assert_eq!(next.active, P1, "reach: P0's turn was skipped");
    assert_eq!((next.triggers, next_face.as_str()), (1, HOWLER));
    let [(c1, c1_face), (control, control_face)] = skip_or_control(false, false);
    assert_eq!((c1.triggers, c1_face.as_str()), (1, SHEPHERD));
    assert_eq!(control.active, P0, "reach: P0's turn was taken");
    assert_eq!((control.triggers, control_face.as_str()), (1, HOWLER));
}

// ===== W10 / W10b: three players ========================================================

/// W10. A non-adjacent third player casting two fires; (1,1,1) does not.
#[test]
fn w10_three_players_the_third_casts_two() {
    let (upkeep, face) = howler_row(&[0, 0, 2], 3);
    assert_eq!((upkeep.triggers, face.as_str()), (1, SHEPHERD));
    let (upkeep, face) = howler_row(&[1, 1, 1], 3);
    assert_eq!((upkeep.triggers, face.as_str()), (0, HOWLER));
}

/// W10b. CR 800.4i: P2 casts two spells in turn N and then leaves the game (P0's Shock, itself one
/// cast, kills it): its actions can still be found, so Howler fires at the next upkeep. Opposite:
/// P1 and P2 cast one spell each before P2 is eliminated: nothing fires.
fn departed_row(p1_casts: usize, p2_casts: usize) -> (Upkeep, String, bool) {
    let (mut b, hands) = board(3, Face::Howler, 3, |_, _| {});
    b.runner.state_mut().players[P2.0 as usize].life = 2;
    for &spell in hands[1].iter().take(p1_casts) {
        cast_at_player(&mut b.runner, P1, spell, P0);
    }
    for &spell in hands[2].iter().take(p2_casts) {
        cast_at_player(&mut b.runner, P2, spell, P0);
    }
    // P0's Shock kills P2 (2 life).
    cast_at_player(&mut b.runner, P0, hands[0][0], P2);
    let eliminated = b.runner.state().players[P2.0 as usize].is_eliminated;
    let upkeep = next_upkeep(&mut b.runner);
    (upkeep, face_name(&b.runner, b.dfc), eliminated)
}

#[test]
fn w10b_a_departed_players_casts_still_count() {
    let (upkeep, face, eliminated) = departed_row(0, 2);
    assert!(eliminated, "reach: P2 left the game in turn N");
    assert_eq!(
        upkeep.triggers, 1,
        "an eliminated player's two casts can still be found"
    );
    assert_eq!(face, SHEPHERD);

    let (upkeep, face, eliminated) = departed_row(1, 1);
    assert!(eliminated, "reach: P2 left the game in turn N");
    assert_eq!(upkeep.triggers, 0, "1 + 1 + 1 is not two by one player");
    assert_eq!(face, HOWLER);
}

// ===== T2 / T2b / T6: saved games across the restore ==========================================

/// Serializes the running state, optionally sets the legacy table-wide key a pre-window build wrote,
/// restores it through `GameState`'s production deserializer and wraps it in a runner.
fn restore(runner: &GameRunner, legacy_total: Option<u64>) -> Result<GameRunner, String> {
    let mut value = serde_json::to_value(runner.state()).expect("state serializes");
    assert!(
        value.get("spells_cast_last_turn").is_none(),
        "a current build writes the per-player snapshot only"
    );
    if let Some(total) = legacy_total {
        value["spells_cast_last_turn"] = serde_json::json!(total);
    }
    let state: GameState = serde_json::from_value(value).map_err(|error| error.to_string())?;
    let mut restored = GameRunner::from_state(state);
    settle(&mut restored);
    Ok(restored)
}

/// T2. A legacy state saved mid-turn N with `"spells_cast_last_turn": 0` and the current turn's
/// per-player records present: the next boundary writes an EXACT per-player snapshot from those
/// records, so Gatstaf Shepherd (front face) transforms at the next upkeep only when nobody cast,
/// and Howler fires on (2,0) but not on (1,1). A defaulted snapshot, or a table-wide zero kept
/// across the boundary, would flip (b) and (c).
#[test]
fn t2_a_legacy_zero_snapshot_still_reads_the_current_turn_exactly() {
    // (a) nobody casts in the rest of N: Shepherd transforms.
    let (b, _hands) = board(2, Face::Shepherd, 3, |_, _| {});
    let mut r = restore(&b.runner, Some(0)).expect("a zero legacy total restores");
    let upkeep = next_upkeep(&mut r);
    assert_eq!(
        (upkeep.triggers, face_name(&r, b.dfc)),
        (1, HOWLER.to_string())
    );

    // (b) P1 casts one spell in the rest of N: Shepherd does not transform.
    let (b, hands) = board(2, Face::Shepherd, 3, |_, _| {});
    let mut r = restore(&b.runner, Some(0)).expect("restores");
    cast_at_player(&mut r, P1, hands[1][0], P0);
    let upkeep = next_upkeep(&mut r);
    assert_eq!(
        (upkeep.triggers, face_name(&r, b.dfc)),
        (0, SHEPHERD.to_string())
    );

    // (c) the Howler face of the same legacy state: (2,0) fires, (1,1) does not.
    let (b, hands) = board(2, Face::Howler, 3, |_, _| {});
    let mut r = restore(&b.runner, Some(0)).expect("restores");
    cast_at_player(&mut r, P0, hands[0][0], P1);
    cast_at_player(&mut r, P0, hands[0][1], P1);
    let upkeep = next_upkeep(&mut r);
    assert_eq!(
        (upkeep.triggers, face_name(&r, b.dfc)),
        (1, SHEPHERD.to_string())
    );
    let (b, hands) = board(2, Face::Howler, 3, |_, _| {});
    let mut r = restore(&b.runner, Some(0)).expect("restores");
    cast_at_player(&mut r, P0, hands[0][0], P1);
    cast_at_player(&mut r, P1, hands[1][0], P0);
    let upkeep = next_upkeep(&mut r);
    assert_eq!(
        (upkeep.triggers, face_name(&r, b.dfc)),
        (0, HOWLER.to_string())
    );
}

/// T2b. CR 603.4: a legacy state saved DURING upkeep with Gatstaf Shepherd's trigger on the stack
/// and `"spells_cast_last_turn": 0`: on resolution the recheck reads the exact (zero) per-player
/// snapshot and Shepherd transforms. The same state with a nonzero total is refused at restore, so
/// a Howler trigger whose legacy total was 2 can never resolve against a defaulted-zero snapshot.
#[test]
fn t2b_a_stack_resident_trigger_survives_a_zero_legacy_restore_and_a_nonzero_one_is_refused() {
    let (mut b, _hands) = board(2, Face::Shepherd, 3, |_, _| {});
    let upkeep = open_next_upkeep(&mut b.runner);
    assert_eq!(
        upkeep.triggers, 1,
        "reach: Shepherd's trigger is on the stack"
    );
    let mut r = restore(&b.runner, Some(0)).expect("a zero legacy total restores mid-upkeep");
    assert_eq!(
        r.state().stack.len(),
        1,
        "the trigger came through the restore"
    );
    resolve_stack(&mut r);
    assert_eq!(
        face_name(&r, b.dfc),
        HOWLER,
        "the recheck read the exact zero snapshot"
    );
    for total in [1, 2] {
        assert!(
            restore(&b.runner, Some(total)).is_err(),
            "total {total} is refused at restore"
        );
    }
}

/// T6. Current-save control: a state saved by the new build after a turn with casts (Howler in
/// play, (2,0)), while the trigger is on the stack, serializes the per-player snapshot and no
/// table-wide key, restores without tripping the legacy refusal, and the trigger still resolves.
#[test]
fn t6_a_new_build_save_restores_and_fires() {
    let (mut b, hands) = board(2, Face::Howler, 3, |_, _| {});
    for &spell in hands[0].iter().take(2) {
        cast_at_player(&mut b.runner, P0, spell, P1);
    }
    let upkeep = open_next_upkeep(&mut b.runner);
    assert_eq!(
        upkeep.triggers, 1,
        "reach: Howler's trigger is on the stack"
    );
    let json = serde_json::to_value(b.runner.state()).expect("serializes");
    assert!(
        json.get("spells_cast_last_turn").is_none(),
        "no table-wide key"
    );
    assert_eq!(
        json["players"][0]["spells_cast_last_turn"], 2,
        "the per-player snapshot is saved"
    );
    let mut r = restore(&b.runner, None).expect("a new-build save restores");
    resolve_stack(&mut r);
    assert_eq!(face_name(&r, b.dfc), SHEPHERD);
}
