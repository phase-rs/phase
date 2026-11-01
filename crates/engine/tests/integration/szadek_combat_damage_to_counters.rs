//! Szadek, Lord of Secrets — "If Szadek would deal combat damage to a player,
//! instead put that many +1/+1 counters on Szadek and that player mills that
//! many cards."
//!
//! CR 614.1a + CR 614.6: a REPLACEMENT (not a CR 615 prevention). The combat
//! damage to the player never happens — no life loss, no `DamageDealt`, no
//! `DamagePrevented` — and the substitute events (counters on Szadek, mill for
//! the damaged player) happen instead. Combat damage to a creature and
//! noncombat damage are dealt normally. With trample, only the portion assigned
//! to the player is replaced (CR 510.1c + CR 702.19b).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CombatDamageAssignmentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SZADEK_NAME: &str = "Szadek, Lord of Secrets";
const SZADEK_ORACLE: &str = "If Szadek would deal combat damage to a player, instead put \
that many +1/+1 counters on Szadek and that player mills that many cards.";

/// Szadek 5/5 attacks P1; `blocker` (if any) blocks it. Returns the runner at
/// the combat-damage step boundary, ready for `combat_damage()`.
fn attack_with_szadek(runner: &mut GameRunner, szadek: ObjectId, blocker: Option<ObjectId>) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(szadek, AttackTarget::Player(P1))])
        .expect("declare attacker");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("advance to blockers");
    }
    // With no potential blocker the engine skips the declare-blockers prompt.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let blocks: Vec<(ObjectId, ObjectId)> = blocker.map(|b| (b, szadek)).into_iter().collect();
        runner.declare_blockers(&blocks).expect("declare blockers");
    } else {
        assert!(blocker.is_none(), "expected a declare-blockers prompt");
    }
}

fn library_count(runner_state: &engine::types::game_state::GameState) -> usize {
    runner_state
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .library
        .len()
}

/// CR 614.6: unblocked, Szadek's 5 combat damage to the defending player is
/// replaced — no life loss, Szadek gets five +1/+1 counters and the damaged
/// player mills five cards.
#[test]
fn unblocked_combat_damage_becomes_counters_and_mill() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0, "replaced damage is never dealt");
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 5);
    assert_eq!(outcome.power_toughness(szadek), (10, 10));
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
    assert_eq!(
        library_count(outcome.state()),
        2,
        "the damaged player mills five"
    );
    assert!(
        !outcome
            .events()
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "a replacement is not a prevention: no DamagePrevented"
    );
    assert!(
        !outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { target, .. } if *target == TargetRef::Player(P1)
        )),
        "no damage is dealt to the player"
    );
}

/// CR 614.1a: the recipient scope is "a player" — combat damage to a blocking
/// creature is dealt normally.
#[test]
fn blocked_combat_damage_to_creature_is_dealt_normally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let wall = scenario.add_creature(P1, "Wall", 0, 7).id();
    scenario.with_library_top(P1, &["C1", "C2", "C3"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, Some(wall));
    let outcome = runner.combat_damage();

    assert_eq!(outcome.damage_marked(wall), 5);
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 0);
    assert_eq!(outcome.life_delta(P1), 0);
    outcome.assert_zone_count(P1, Zone::Graveyard, 0);
    assert_eq!(library_count(outcome.state()), 3);
}

/// CR 702.19b + CR 614.1a: a trampling Szadek blocked by a 0/2 assigns 2 to
/// the blocker (dealt normally) and 3 to the player — only the player portion
/// is replaced (3 counters, mill 3).
#[test]
fn trample_replaces_only_the_player_portion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .trample()
        .id();
    let wall = scenario.add_creature(P1, "Wall", 0, 2).id();
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, Some(wall));
    // CR 510.1c: the trampler's controller assigns lethal (2) to the 0/2 wall
    // and the remaining 3 to the player.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::AssignCombatDamage { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("advance to damage assignment");
    }
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: vec![(wall, 2)],
            trample_damage: 3,
            controller_damage: 0,
        })
        .expect("assign trample damage");
    let outcome = runner.combat_damage();

    assert_eq!(
        outcome.state().players[1].life,
        20,
        "the player portion is never dealt"
    );
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 3);
    assert_eq!(library_count(outcome.state()), 2);
    // The 0/2 wall took lethal damage and died; with the 3 milled cards, P1's
    // graveyard holds four cards.
    outcome.assert_zone_count(P1, Zone::Graveyard, 4);
}

/// CR 614.1a + CR 616.1: "that many" names the replaced damage amount for BOTH
/// substitute events. A counter modifier (Hardened Scales) changes how many
/// counters Szadek gets (5 + 1) but not how many cards the player mills (5).
#[test]
fn counter_modifier_does_not_change_mill_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Hardened Scales Bearer",
        0,
        1,
        "If one or more +1/+1 counters would be put on a creature you control, that many \
         plus one +1/+1 counters are put on it instead.",
    );
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0);
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 6);
    assert_eq!(library_count(outcome.state()), 2, "mill that many = 5");
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
}

/// CR 614.1a + CR 608.2c: the mill reads the replaced damage amount, not how
/// many counters were actually put. When no counter can be put on Szadek, the
/// damaged player still mills five.
#[test]
fn mill_amount_holds_when_no_counter_can_be_put() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(
        P1,
        "Counter Ban",
        "Counters can't be put on artifacts, creatures, enchantments, or lands.",
    );
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    attack_with_szadek(&mut runner, szadek, None);
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0, "replaced damage is never dealt");
    outcome.assert_counters(szadek, CounterType::Plus1Plus1, 0);
    assert_eq!(library_count(outcome.state()), 2, "mill that many = 5");
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
}

/// CR 510.2 + CR 614.5: two Szadeks' combat damage events in the same batch are
/// each replaced — each Szadek gets its own counters and the player mills for
/// both events.
#[test]
fn two_szadeks_in_one_damage_batch_are_each_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 3, 3, SZADEK_ORACLE)
        .id();
    let library: Vec<String> = (1..=10).map(|i| format!("C{i}")).collect();
    let library: Vec<&str> = library.iter().map(String::as_str).collect();
    scenario.with_library_top(P1, &library);

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    let outcome = runner.combat_damage();

    assert_eq!(outcome.life_delta(P1), 0);
    outcome.assert_counters(first, CounterType::Plus1Plus1, 5);
    outcome.assert_counters(second, CounterType::Plus1Plus1, 3);
    assert_eq!(library_count(outcome.state()), 2, "mill 5 + mill 3");
    outcome.assert_zone_count(P1, Zone::Graveyard, 8);
}

// ---------------------------------------------------------------------------
// CR 616.1: choices inside the simultaneous combat-damage batch.
//
// CR 510.2 forbids casting spells and activating abilities between combat
// damage being assigned and dealt. It does not remove the CR 616.1 choice of
// which replacement applies first, nor a choice raised by an event contained
// in the substitute (CR 616.1g: counters, mill). The batch parks on those
// prompts and resumes once they are answered; every test below answers through
// real `GameAction`s.
// ---------------------------------------------------------------------------

const FURNACE_ORACLE: &str = "If a source would deal damage to a permanent or player, it \
deals double that damage to that permanent or player instead.";
const COMBAT_WARD_ORACLE: &str = "Prevent all combat damage that would be dealt to you.";
const HARDENED_SCALES_ORACLE: &str = "If one or more +1/+1 counters would be put on a \
creature you control, that many plus one +1/+1 counters are put on it instead.";
const DOUBLING_SEASON_ORACLE: &str = "If an effect would create one or more tokens under \
your control, it creates twice that many of those tokens instead.\nIf an effect would put \
one or more counters on a permanent you control, it puts twice that many of those counters \
on that permanent instead.";
const REST_IN_PEACE_ORACLE: &str =
    "If a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_OF_THE_VOID_ORACLE: &str =
    "If a card would be put into an opponent's graveyard from anywhere, exile it instead.";

fn library_of(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("C{i}")).collect()
}

fn with_p1_library(scenario: &mut GameScenario, n: usize) {
    let names = library_of(n);
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_library_top(P1, &names);
}

/// Declare `attackers` at P1 in this order (the batch's assignment order) and
/// pass priority until the combat-damage step raises a prompt that is not a
/// priority window, or combat ends.
fn attack_and_run_to_prompt(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.advance_to_combat();
    let declared: Vec<(ObjectId, AttackTarget)> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&declared)
        .expect("declare attackers");
    run_to_prompt(runner);
}

fn run_to_prompt(runner: &mut GameRunner) {
    for _ in 0..32 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {}
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            _ => return,
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
}

/// Answer the open CR 616.1 prompt by choosing the candidate created by
/// `source`, asserting who is asked.
fn choose_replacement_from(runner: &mut GameRunner, chooser: PlayerId, source: ObjectId) {
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a CR 616.1 replacement choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, chooser,
        "CR 616.1: the affected player/controller chooses"
    );
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == source)
        .unwrap_or_else(|| panic!("no candidate from {source:?} in {candidates:?}"));
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the replacement choice is legal");
}

/// Serialize the whole game while the batch is parked, restore it, and keep
/// playing on the restored state: the parked batch must survive persistence.
fn round_trip_at_pause(runner: &mut GameRunner) {
    let waiting_before = runner.state().waiting_for.clone();
    let json = serde_json::to_string(runner.state()).expect("serialize parked state");
    let restored: GameState = serde_json::from_str(&json).expect("deserialize parked state");
    assert_eq!(
        restored.waiting_for, waiting_before,
        "the open prompt survives persistence"
    );
    *runner.state_mut() = restored;
}

fn counters_on(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn zone_len(runner: &GameRunner, zone: Zone) -> usize {
    let player = &runner.state().players[1];
    match zone {
        Zone::Library => player.library.len(),
        Zone::Graveyard => player.graveyard.len(),
        Zone::Exile => runner
            .state()
            .exile
            .iter()
            .filter(|id| {
                runner
                    .state()
                    .objects
                    .get(id)
                    .is_some_and(|o| o.owner == P1)
            })
            .count(),
        other => panic!("unexpected zone {other:?}"),
    }
}

/// CR 510.2: the answer to the batch's last prompt completes the batch itself,
/// in the same action — no priority window opens before the damage is dealt.
fn assert_batch_completed_by_the_answer(runner: &GameRunner) {
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "the answering action resumes and completes the parked batch"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.regular_damage_done),
        "combat damage is marked done before anyone receives priority"
    );
}

/// Finish combat after the last prompt and check that nothing of the batch is
/// still parked.
fn finish_combat(runner: &mut GameRunner) {
    run_to_prompt(runner);
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat damage finished; left at {:?} waiting for {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "no batch remains parked"
    );
}

/// Szadek 5/5 then a 2/2 attack P1, who controls Furnace of Rath.
fn furnace_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let furnace = scenario
        .add_enchantment_from_oracle(P1, "Furnace of Rath", FURNACE_ORACLE)
        .id();
    with_p1_library(&mut scenario, 12);
    let mut runner = scenario.build();
    attack_and_run_to_prompt(&mut runner, &[szadek, bear]);
    (runner, szadek, furnace)
}

/// CR 616.1 + CR 614.6: Furnace of Rath applied first doubles Szadek's 5 to
/// 10, and Szadek's substitution then replaces those 10: ten counters, mill
/// ten. The defending player — the affected player — makes the choice. The
/// batch parks on it, survives a save/restore, and resumes the 2/2 behind it
/// exactly once (Furnace doubles it to 4, no choice there).
#[test]
fn defender_orders_furnace_first_ten_counters_and_mill_ten() {
    let (mut runner, szadek, furnace) = furnace_board();
    round_trip_at_pause(&mut runner);
    choose_replacement_from(&mut runner, P1, furnace);
    assert_batch_completed_by_the_answer(&runner);
    finish_combat(&mut runner);

    assert_eq!(
        counters_on(&runner, szadek),
        10,
        "5 doubled, then substituted"
    );
    assert_eq!(zone_len(&runner, Zone::Library), 2, "mills that many = 10");
    assert_eq!(zone_len(&runner, Zone::Graveyard), 10);
    assert_eq!(
        runner.state().players[1].life,
        16,
        "only the 2/2's doubled 4 is dealt, once"
    );
}

/// CR 616.1 + CR 614.6: Szadek's substitution applied first replaces the 5
/// damage: five counters, mill five. The damage event no longer exists, so
/// Furnace of Rath has nothing left to double.
#[test]
fn defender_orders_szadek_first_five_counters_and_mill_five() {
    let (mut runner, szadek, _furnace) = furnace_board();
    choose_replacement_from(&mut runner, P1, szadek);
    assert_batch_completed_by_the_answer(&runner);
    finish_combat(&mut runner);

    assert_eq!(counters_on(&runner, szadek), 5);
    assert_eq!(zone_len(&runner, Zone::Library), 7, "mills that many = 5");
    assert_eq!(zone_len(&runner, Zone::Graveyard), 5);
    assert_eq!(runner.state().players[1].life, 16);
}

/// CR 616.1 + CR 615: against "Prevent all combat damage that would be dealt
/// to you" the defending player orders the two. Prevention first: nothing is
/// dealt and nothing is substituted. Substitution first: five counters, mill
/// five, and the prevention has no damage left to prevent.
#[test]
fn defender_orders_prevention_and_substitution_both_ways() {
    for prevention_first in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let szadek = scenario
            .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
            .id();
        let ward = scenario
            .add_enchantment_from_oracle(P1, "Combat Ward", COMBAT_WARD_ORACLE)
            .id();
        with_p1_library(&mut scenario, 7);
        let mut runner = scenario.build();
        attack_and_run_to_prompt(&mut runner, &[szadek]);
        choose_replacement_from(
            &mut runner,
            P1,
            if prevention_first { ward } else { szadek },
        );
        finish_combat(&mut runner);

        assert_eq!(runner.state().players[1].life, 20, "no damage either way");
        let (counters, library) = if prevention_first { (0, 7) } else { (5, 2) };
        assert_eq!(
            counters_on(&runner, szadek),
            counters,
            "prevention_first={prevention_first}"
        );
        assert_eq!(
            zone_len(&runner, Zone::Library),
            library,
            "prevention_first={prevention_first}"
        );
    }
}

/// A 2/2 then Szadek attack; P0 controls Hardened Scales and Doubling Season.
fn counter_order_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_ORACLE)
        .id();
    let season = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
        .id();
    with_p1_library(&mut scenario, 7);
    let mut runner = scenario.build();
    attack_and_run_to_prompt(&mut runner, &[bear, szadek]);
    (runner, szadek, scales, season)
}

/// CR 616.1 + CR 616.1g: the substitute's counter event meets Hardened Scales
/// and Doubling Season, and Szadek's controller orders them. Scales first:
/// (5 + 1) × 2 = 12. Season first: 5 × 2 + 1 = 11. The batch parks on that
/// inner prompt, survives a save/restore, and finishes exactly once: the mill
/// still reads the replaced damage (5) and the 2/2 that came first in the
/// batch deals its 2 once.
#[test]
fn counter_order_inside_the_substitute_parks_the_batch_both_orders() {
    for scales_first in [true, false] {
        let (mut runner, szadek, scales, season) = counter_order_board();
        round_trip_at_pause(&mut runner);
        choose_replacement_from(&mut runner, P0, if scales_first { scales } else { season });
        assert_batch_completed_by_the_answer(&runner);
        finish_combat(&mut runner);

        assert_eq!(
            counters_on(&runner, szadek),
            if scales_first { 12 } else { 11 },
            "scales_first={scales_first}"
        );
        assert_eq!(
            zone_len(&runner, Zone::Library),
            2,
            "mills that many = 5, once"
        );
        assert_eq!(zone_len(&runner, Zone::Graveyard), 5);
        assert_eq!(runner.state().players[1].life, 18, "the 2/2's damage, once");
    }
}

/// CR 616.1 + CR 616.1g + CR 701.17a: every card the substitute mills meets
/// Rest in Peace and Leyline of the Void, so each card's move parks on its own
/// ordering prompt. The batch stays parked across all of them (saved and
/// restored at the first), then resumes once: five counters, five cards in
/// exile, and the 2/2 behind Szadek deals its 2 once.
#[test]
fn mill_replacement_order_inside_the_substitute_parks_the_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Leyline of the Void", LEYLINE_OF_THE_VOID_ORACLE);
    with_p1_library(&mut scenario, 7);
    let mut runner = scenario.build();
    attack_and_run_to_prompt(&mut runner, &[szadek, bear]);

    let mut prompts = 0usize;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        if prompts == 0 {
            round_trip_at_pause(&mut runner);
        }
        prompts += 1;
        assert!(prompts <= 16, "the mill prompts terminate");
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("the per-card ordering choice is legal");
    }
    assert_eq!(
        prompts, 5,
        "two graveyard diverts raise one ordering prompt per milled card, each once"
    );
    assert_batch_completed_by_the_answer(&runner);
    finish_combat(&mut runner);

    assert_eq!(counters_on(&runner, szadek), 5);
    assert_eq!(
        zone_len(&runner, Zone::Library),
        2,
        "mills that many = 5, once"
    );
    assert_eq!(zone_len(&runner, Zone::Graveyard), 0);
    assert_eq!(
        zone_len(&runner, Zone::Exile),
        5,
        "every milled card is exiled"
    );
    assert_eq!(runner.state().players[1].life, 18, "the 2/2's damage, once");
}

/// CR 800.4a + CR 616.1: the defending player leaves the game while the batch
/// waits on their ordering choice. No answer can arrive, so their Szadek
/// event is not dealt or substituted, and the rest of the batch still
/// completes once: the 2/2 attacking the third player deals its 2 (Furnace
/// of Rath left the game with its owner, so nothing doubles it).
#[test]
fn chooser_leaving_the_game_releases_the_parked_batch() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.add_enchantment_from_oracle(P1, "Furnace of Rath", FURNACE_ORACLE);
    with_p1_library(&mut scenario, 7);
    let p2 = PlayerId(2);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (szadek, AttackTarget::Player(P1)),
            (bear, AttackTarget::Player(p2)),
        ])
        .expect("declare attackers");
    run_to_prompt(&mut runner);
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected P1's CR 616.1 prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "reach guard: the defending player orders");

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("CR 800.4: a player may leave at any time");
    for _ in 0..16 {
        if runner.state().pending_combat_lifelink.is_none() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("no further prompt is owed after the departure: {other:?}"),
        }
    }
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "the parked batch drains instead of stranding"
    );
    assert_eq!(
        counters_on(&runner, szadek),
        0,
        "the departed seat's event is not substituted"
    );
    assert_eq!(
        runner.state().players[2].life,
        18,
        "the rest of the batch still applies, once"
    );
}

/// CR 510.4 + CR 616.1: a first-strike Szadek parks the FIRST combat-damage
/// sub-step on the defender's choice. Answering it completes that sub-step
/// (ten counters, mill ten), and the mandatory regular sub-step still runs
/// once afterwards: the 2/2 without first strike deals its doubled 4.
#[test]
fn first_strike_batch_parks_and_the_regular_sub_step_still_runs_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .first_strike()
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let furnace = scenario
        .add_enchantment_from_oracle(P1, "Furnace of Rath", FURNACE_ORACLE)
        .id();
    with_p1_library(&mut scenario, 12);
    let mut runner = scenario.build();
    attack_and_run_to_prompt(&mut runner, &[szadek, bear]);
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| !combat.first_strike_done),
        "reach guard: the FIRST-STRIKE sub-step is the one that parks"
    );
    assert_eq!(
        runner.state().players[1].life,
        20,
        "nothing is dealt before the answer"
    );

    choose_replacement_from(&mut runner, P1, furnace);
    finish_combat(&mut runner);

    assert_eq!(counters_on(&runner, szadek), 10);
    assert_eq!(zone_len(&runner, Zone::Library), 2);
    assert_eq!(
        runner.state().players[1].life,
        16,
        "the regular sub-step runs once: the 2/2's doubled 4"
    );
}

/// CR 800.4a + CR 616.1: a seat that is NOT the chooser leaves while the
/// batch waits in its replacement pass. Its lifelink blocker's gain has not
/// been computed yet (Phase C derives it from the parked entry), so the
/// departure must drop it there: the departed seat gains no life, and the
/// defender's answer still completes the batch once.
#[test]
fn departed_seat_gains_no_life_from_a_batch_parked_in_its_replacement_pass() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, SZADEK_NAME, 5, 5, SZADEK_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 1, 1).id();
    let p2 = PlayerId(2);
    let lifelinker = {
        let mut builder = scenario.add_creature(p2, "Lifelink Wall", 1, 4);
        builder.with_keyword(engine::types::keywords::Keyword::Lifelink);
        builder.id()
    };
    scenario.add_enchantment_from_oracle(P1, "Combat Ward", COMBAT_WARD_ORACLE);
    with_p1_library(&mut scenario, 7);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (szadek, AttackTarget::Player(P1)),
            (bear, AttackTarget::Player(p2)),
        ])
        .expect("declare attackers");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { player, .. } if *player == p2 => {
                runner
                    .declare_blockers(&[(lifelinker, bear)])
                    .expect("P2 blocks the bear");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::ReplacementChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected P1's CR 616.1 prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P1,
        "reach guard: the batch parks on the defender's choice"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&lifelinker)),
        "reach guard: the lifelink wall blocks"
    );
    let p2_life = runner.state().players[2].life;

    runner
        .act(GameAction::Concede { player_id: p2 })
        .expect("CR 800.4: a player may leave at any time");
    choose_replacement_from(&mut runner, P1, szadek);
    assert_batch_completed_by_the_answer(&runner);

    assert_eq!(
        runner.state().players[2].life,
        p2_life,
        "CR 800.4a: the departed seat gains no life"
    );
    assert_eq!(
        counters_on(&runner, szadek),
        5,
        "the defender's answer still applies"
    );
}

const TORBRAN_ORACLE: &str = "If a red source you control would deal damage to an opponent or \
a permanent an opponent controls, it deals that much damage plus 2 instead.";

/// CR 616.1 + CR 510.2 + CR 704.6c: the ordering choice of a batch event that
/// SURVIVES its replacements (two damage modifications, no substitution) is
/// answered inside the batch, so the survivor is applied by Phase C with the
/// batch's own bookkeeping. P1 orders Torbran's +2 against Furnace of Rath's
/// doubling for a red 2/2 commander: Torbran first → (2 + 2) × 2 = 8, Furnace
/// first → 2 × 2 + 2 = 6. Both the life loss and the commander-damage tally
/// carry the chosen amount.
#[test]
fn surviving_event_ordered_by_defender_keeps_combat_bookkeeping() {
    for torbran_first in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let commander = {
            let mut builder = scenario.add_creature(P0, "Red Commander", 2, 2);
            builder
                .commander()
                .with_color(vec![engine::types::mana::ManaColor::Red]);
            builder.id()
        };
        let torbran = scenario
            .add_creature_from_oracle(P0, "Torbran Stand-In", 6, 4, TORBRAN_ORACLE)
            .id();
        let furnace = scenario
            .add_enchantment_from_oracle(P1, "Furnace of Rath", FURNACE_ORACLE)
            .id();
        let mut runner = scenario.build();
        attack_and_run_to_prompt(&mut runner, &[commander]);
        choose_replacement_from(
            &mut runner,
            P1,
            if torbran_first { torbran } else { furnace },
        );
        assert_batch_completed_by_the_answer(&runner);
        finish_combat(&mut runner);

        let dealt: u32 = if torbran_first { 8 } else { 6 };
        assert_eq!(
            runner.state().players[1].life,
            20 - dealt as i32,
            "torbran_first={torbran_first}"
        );
        let tally: u32 = runner
            .state()
            .commander_damage
            .iter()
            .filter(|entry| entry.player == P1 && entry.commander == commander)
            .map(|entry| entry.damage)
            .sum();
        assert_eq!(
            tally, dealt,
            "CR 704.6c: Phase C books the chosen amount as commander damage"
        );
    }
}
