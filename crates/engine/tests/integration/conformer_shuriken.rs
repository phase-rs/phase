//! Conformer Shuriken — runtime tests for the granted attack trigger.
//!
//! Oracle (Scryfall, verified 2026-09-29), Legendary Artifact — Equipment {2}:
//!   Equipped creature has "Whenever this creature attacks, tap target creature
//!   defending player controls. If that creature has greater power than this
//!   creature, put a number of +1/+1 counters on this creature equal to the
//!   difference."
//!   Equip {2}
//!
//! Every test drives the real declare-attackers → trigger → stack pipeline. The
//! reach guards are that the trigger's stack entry targets the expected
//! creature, and that resolution actually ran (the target got tapped, or the
//! stack drained after a response). "No counters" is never asserted without
//! one of them.
//!
//! Response spells are the printed cards' Oracle text, cast through
//! `GameRunner::cast` at P0's priority while the trigger waits on the stack.
//! Their mana costs are left unset: the tests are about stack timing, not payment.

use engine::game::ability_utils::validate_targets_in_chain;
use engine::game::derived_views::derive_views;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const SHURIKEN: &str = "Equipped creature has \"Whenever this creature attacks, tap target creature defending player controls. If that creature has greater power than this creature, put a number of +1/+1 counters on this creature equal to the difference.\"\nEquip {2}";

const LESS_TOUGHNESS_SHURIKEN: &str = "Equipped creature has \"Whenever this creature attacks, tap target creature defending player controls. If that creature has less toughness than this creature, put a number of +1/+1 counters on this creature equal to the difference.\"\nEquip {2}";

const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

struct Board {
    runner: GameRunner,
    attacker: ObjectId,
    shuriken: ObjectId,
    defender: ObjectId,
    response: Option<ObjectId>,
}

/// P0's `attacker` (power/toughness given) is equipped with a Shuriken built from
/// `oracle`; P1 controls `defender`. `response` optionally puts one instant in
/// P0's hand.
fn board(
    oracle: &str,
    attacker_pt: (i32, i32),
    defender_pt: (i32, i32),
    response: Option<(&str, &str)>,
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Equipped Attacker", attacker_pt.0, attacker_pt.1)
        .id();
    let shuriken = scenario
        .add_artifact_from_oracle(P0, "Conformer Shuriken", oracle)
        .with_subtypes(vec!["Equipment"])
        .id();
    let defender = scenario
        .add_creature(P1, "Defending Creature", defender_pt.0, defender_pt.1)
        .id();
    let response = response.map(|(name, text)| {
        scenario
            .add_spell_to_hand_from_oracle(P0, name, true, text)
            .id()
    });
    let mut runner = scenario.build();
    // CR 301.5a: the Shuriken equips the attacker (what Equip's resolution does).
    let state = runner.state_mut();
    state.objects.get_mut(&shuriken).unwrap().attached_to = Some(AttachTarget::Object(attacker));
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .attachments
        .push(shuriken);
    Board {
        runner,
        attacker,
        shuriken,
        defender,
        response,
    }
}

/// CR 508.1m: declare the attack, answer the granted trigger's target prompt
/// (if any) with `target`, and return with the trigger on the stack.
/// REACH GUARD: the trigger's stack entry targets `target`.
fn attack_and_target(b: &mut Board, target: ObjectId) {
    b.runner.advance_to_combat();
    b.runner
        .declare_attackers(&[(b.attacker, AttackTarget::Player(P1))])
        .expect("the equipped creature attacks");
    for _ in 0..8 {
        match b.runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                b.runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert_eq!(
                    target_slots.len(),
                    1,
                    "CR 115.1: only the tap names a target; the gated clause reads it: {target_slots:?}"
                );
                b.runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose the tap target");
            }
            _ => break,
        }
    }
    let entry = b
        .runner
        .state()
        .stack
        .back()
        .expect("reach guard: the granted attack trigger is on the stack");
    let ability = entry.ability().expect("a triggered ability");
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(target)],
        "reach guard: the trigger targets the chosen creature"
    );
    assert_eq!(
        entry.source_id, b.attacker,
        "CR 113.7: the granted trigger's source is the equipped creature"
    );
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 208.1 + CR 608.2c: a 2/2 attacker taps a 5/5 and gets 5 − 2 = 3 counters;
/// the Equipment gets none (CR 113.7: "this creature" is the trigger's source).
#[test]
fn bigger_target_puts_the_difference_on_the_equipped_creature() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&b.defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 3);
    assert_eq!(p1p1(&b.runner, b.shuriken), 0);
}

/// "Greater" is strict: equal power puts no counters.
#[test]
fn equal_power_puts_no_counters() {
    let mut b = board(SHURIKEN, (3, 3), (3, 3), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&b.defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

/// A smaller target puts no counters (and no negative-difference artifact).
#[test]
fn smaller_target_puts_no_counters() {
    let mut b = board(SHURIKEN, (4, 4), (2, 2), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&b.defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

/// The gate reads power, not whether the tap changed anything: an already
/// tapped 5/5 is still a legal target and still yields 3.
#[test]
fn already_tapped_target_still_counts() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), None);
    b.runner
        .state_mut()
        .objects
        .get_mut(&b.defender)
        .unwrap()
        .tapped = true;
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().stack.is_empty(),
        "reach guard: the trigger resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 3);
}

/// CR 608.2b: printed Unsummon returns the only target in response, so the
/// trigger doesn't resolve at all: no counters.
#[test]
fn target_leaving_before_resolution_does_nothing() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), Some(("Unsummon", UNSUMMON)));
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    let unsummon = b.response.unwrap();
    b.runner.cast(unsummon).target_object(defender).resolve();
    assert_eq!(
        b.runner.state().objects[&defender].zone,
        Zone::Hand,
        "reach guard: Unsummon resolved"
    );
    assert!(
        b.runner.state().stack.is_empty(),
        "reach guard: stack drained"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

/// CR 608.2h: printed Giant Growth on the 5/5 in response makes it 8/8 at
/// resolution; the 2/2 attacker gets 8 − 2 = 6.
#[test]
fn giant_growth_on_the_target_in_response_raises_the_difference() {
    let mut b = board(
        SHURIKEN,
        (2, 2),
        (5, 5),
        Some(("Giant Growth", GIANT_GROWTH)),
    );
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    let growth = b.response.unwrap();
    b.runner.cast(growth).target_object(defender).resolve();
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 6);
}

/// CR 608.2h: printed Giant Growth on the 2/2 attacker (→ 5/5) against a 4/4
/// removes the gap: no counters.
#[test]
fn giant_growth_on_the_attacker_in_response_removes_the_counters() {
    let mut b = board(
        SHURIKEN,
        (2, 2),
        (4, 4),
        Some(("Giant Growth", GIANT_GROWTH)),
    );
    let (attacker, defender) = (b.attacker, b.defender);
    attack_and_target(&mut b, defender);
    let growth = b.response.unwrap();
    b.runner.cast(growth).target_object(attacker).resolve();
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(
        b.runner.state().objects[&attacker].power,
        Some(5),
        "reach guard: Giant Growth resolved on the attacker"
    );
    assert_eq!(p1p1(&b.runner, attacker), 0);
}

/// CR 113.7a: printed Disenchant destroys the Shuriken with the trigger on the
/// stack; the ability exists independently of it, so the attacker still gets 3.
#[test]
fn destroying_the_shuriken_in_response_does_not_stop_the_trigger() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), Some(("Disenchant", DISENCHANT)));
    let (attacker, defender, shuriken) = (b.attacker, b.defender, b.shuriken);
    attack_and_target(&mut b, defender);
    let disenchant = b.response.unwrap();
    b.runner.cast(disenchant).target_object(shuriken).resolve();
    assert_eq!(
        b.runner.state().objects[&shuriken].zone,
        Zone::Graveyard,
        "reach guard: Disenchant resolved"
    );
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, attacker), 3);
    assert_eq!(p1p1(&b.runner, shuriken), 0);
}

/// Accepted-shape runtime control (`less toughness`): a 2/5 attacker against a
/// 3/2 target gets 5 − 2 = 3; with the target at toughness 5 (equal), none.
#[test]
fn less_toughness_variant_runs_end_to_end() {
    let mut b = board(LESS_TOUGHNESS_SHURIKEN, (2, 5), (3, 2), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 3);

    let mut control = board(LESS_TOUGHNESS_SHURIKEN, (2, 5), (3, 5), None);
    let defender = control.defender;
    attack_and_target(&mut control, defender);
    control.runner.advance_until_stack_empty();
    assert!(
        control.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&control.runner, control.attacker), 0);
}

// ---------------------------------------------------------------------------
// Class boards (review round 1): the same gate on printed-trigger creatures,
// population bodies, modal chains, and partially illegal chains.
// ---------------------------------------------------------------------------

const C1_TEXT: &str = "Whenever this creature attacks, tap target creature defending player controls. If that creature has greater power than this creature, return each creature you control to its owner's hand.";
const C2_BOTH: &str = "Whenever this creature attacks, choose one or both —\n• Tap target creature defending player controls. If that creature has greater power than this creature, put a number of +1/+1 counters on this creature equal to the difference.\n• Tap target creature defending player controls. If that creature has greater power than this creature, put a number of +1/+1 counters on this creature equal to the difference.";
const C2_REPEAT: &str = "Whenever this creature attacks, choose two. You may choose the same mode more than once.\n• Tap target creature defending player controls. If that creature has greater power than this creature, put a number of +1/+1 counters on this creature equal to the difference.\n• You gain 1 life.";

/// P0 controls `Attacker` (printed `text`) and a 1/1; P1 controls one creature
/// per entry of `defenders`.
struct PrintedBoard {
    runner: GameRunner,
    attacker: ObjectId,
    other: ObjectId,
    defenders: Vec<ObjectId>,
    unsummon: ObjectId,
}

fn printed_board(text: &str, attacker_pt: (i32, i32), defenders: &[(i32, i32)]) -> PrintedBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(P0, "Attacker", attacker_pt.0, attacker_pt.1, text)
        .id();
    let other = scenario.add_creature(P0, "Other Mine", 1, 1).id();
    let defenders = defenders
        .iter()
        .enumerate()
        .map(|(i, &(p, t))| {
            scenario
                .add_creature(P1, &format!("Defender {i}"), p, t)
                .id()
        })
        .collect();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    PrintedBoard {
        runner: scenario.build(),
        attacker,
        other,
        defenders,
        unsummon,
    }
}

/// CR 508.1m + CR 700.2 + CR 603.3d: declare the attack, choose `modes`, and
/// answer each target slot in order with `picks`. Returns the slot count seen.
/// REACH GUARD: every pick was a legal choice of its slot.
fn attack_with_modes(b: &mut PrintedBoard, modes: &[usize], picks: &[ObjectId]) -> usize {
    b.runner.advance_to_combat();
    b.runner
        .declare_attackers(&[(b.attacker, AttackTarget::Player(P1))])
        .expect("the attacker attacks");
    let mut slots_seen = 0;
    let mut next_pick = 0;
    for _ in 0..16 {
        match b.runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { .. } | WaitingFor::ModeChoice { .. } => {
                b.runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("select modes");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                b.runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                slots_seen = target_slots.len();
                let pick = TargetRef::Object(picks[next_pick]);
                assert!(
                    target_slots[selection.current_slot]
                        .legal_targets
                        .contains(&pick),
                    "reach guard: pick {next_pick} is legal for its slot: {target_slots:?}"
                );
                next_pick += 1;
                b.runner
                    .act(GameAction::ChooseTarget { target: Some(pick) })
                    .expect("choose target");
            }
            _ => break,
        }
    }
    assert!(
        !b.runner.state().stack.is_empty(),
        "reach guard: the attack trigger is on the stack"
    );
    slots_seen
}

/// CR 608.2c: the gate over a POPULATION body ("return each creature you
/// control") reads the tapped target; when it holds, every creature P0 controls
/// returns. Control: a 1/1 target leaves the gate false and both stay.
#[test]
fn comparative_gate_over_a_population_body_resolves() {
    let mut b = printed_board(C1_TEXT, (2, 2), &[(5, 5)]);
    let target = b.defenders[0];
    attack_with_modes(&mut b, &[], &[target]);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&target].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(b.runner.state().objects[&b.attacker].zone, Zone::Hand);
    assert_eq!(b.runner.state().objects[&b.other].zone, Zone::Hand);

    let mut control = printed_board(C1_TEXT, (2, 2), &[(1, 1)]);
    let target = control.defenders[0];
    attack_with_modes(&mut control, &[], &[target]);
    control.runner.advance_until_stack_empty();
    assert!(
        control.runner.state().objects[&target].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(
        control.runner.state().objects[&control.attacker].zone,
        Zone::Battlefield
    );
    assert_eq!(
        control.runner.state().objects[&control.other].zone,
        Zone::Battlefield
    );
}

/// CR 700.2 + CR 608.2c: "choose one or both" with two tap-and-compare modes.
/// Each mode reads ITS OWN tapped creature: +3 from the 5/5 (attacker becomes
/// 5/5), then +2 from the 7/7 = 5. Two slots, no panic.
#[test]
fn modal_one_or_both_each_mode_reads_its_own_tap() {
    let mut b = printed_board(C2_BOTH, (2, 2), &[(5, 5), (7, 7)]);
    let (five, seven) = (b.defenders[0], b.defenders[1]);
    let slots = attack_with_modes(&mut b, &[0, 1], &[five, seven]);
    assert_eq!(slots, 2, "one slot per mode's tap, none for the riders");
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&five].tapped,
        "reach guard: mode 1 tap"
    );
    assert!(
        b.runner.state().objects[&seven].tapped,
        "reach guard: mode 2 tap"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 5);
}

/// CR 700.2d: the same mode chosen twice; each occurrence reads its own tap:
/// +3 (5/5), then +2 (7/7 vs a 5/5 attacker) = 5.
#[test]
fn modal_same_mode_twice_reads_each_occurrences_tap() {
    let mut b = printed_board(C2_REPEAT, (2, 2), &[(5, 5), (7, 7)]);
    let (five, seven) = (b.defenders[0], b.defenders[1]);
    let slots = attack_with_modes(&mut b, &[0, 0], &[five, seven]);
    assert_eq!(slots, 2, "one slot per occurrence's tap");
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&five].tapped,
        "reach guard: first tap"
    );
    assert!(
        b.runner.state().objects[&seven].tapped,
        "reach guard: second tap"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 5);
}

/// CR 608.2b: the first mode's tap target leaves in response (printed
/// Unsummon), the second mode's is still legal, so the ability resolves. The
/// first gate needs information about an illegal target, so it does nothing;
/// the second reads its own 4/4: 4 − 2 = +2. (Reading the stale 5/5 would give
/// +3 and then 4 > 5 false = 3.)
#[test]
fn modal_first_tap_target_bounced_later_mode_still_resolves() {
    let mut b = printed_board(C2_BOTH, (2, 2), &[(5, 5), (4, 4)]);
    let (five, four, unsummon) = (b.defenders[0], b.defenders[1], b.unsummon);
    attack_with_modes(&mut b, &[0, 1], &[five, four]);
    b.runner.cast(unsummon).target_object(five).resolve();
    assert_eq!(
        b.runner.state().objects[&five].zone,
        Zone::Hand,
        "reach guard: Unsummon resolved"
    );
    assert!(
        b.runner.state().stack.is_empty(),
        "reach guard: stack drained"
    );
    assert!(
        b.runner.state().objects[&four].tapped,
        "reach guard: the later mode resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 2);
}

/// CR 608.2b anti-zero: an illegal target's power is not information, so it is
/// not read as 0. Attacker −1/5; the first tap target (−2/5) is bounced, the
/// second is a −1/5. Correct: mode 1 does nothing, mode 2's gate is −1 > −1,
/// false: 0 counters. Reading the missing power as 0 would make mode 1's gate
/// 0 > −1 true (+1).
#[test]
fn bounced_first_target_is_not_read_as_zero_power() {
    let mut b = printed_board(C2_BOTH, (-1, 5), &[(-2, 5), (-1, 5)]);
    let (first, second, unsummon) = (b.defenders[0], b.defenders[1], b.unsummon);
    attack_with_modes(&mut b, &[0, 1], &[first, second]);
    b.runner.cast(unsummon).target_object(first).resolve();
    assert_eq!(
        b.runner.state().objects[&first].zone,
        Zone::Hand,
        "reach guard: Unsummon resolved"
    );
    assert!(
        b.runner.state().objects[&second].tapped,
        "reach guard: the later mode resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

// ---------------------------------------------------------------------------
// #3864 GainLife rider sibling: the inheriting-rider descent and the validated
// parent restamp, on Swords to Plowshares' exact instruction inside a modal.
// ---------------------------------------------------------------------------

const SWORDS_MODAL: &str = "Choose one or both —\n• Exile target creature. Its controller gains life equal to its power.\n• Target player draws a card.";

struct SwordsBoard {
    runner: GameRunner,
    spell: ObjectId,
    four: ObjectId,
    unsummon: ObjectId,
}

fn swords_board() -> SwordsBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords Modal", true, SWORDS_MODAL)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let four = scenario.add_creature(P1, "P1 Four", 4, 4).id();
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
    }
    SwordsBoard {
        runner: scenario.build(),
        spell,
        four,
        unsummon,
    }
}

/// CR 601.2c + CR 700.2: the life-gain rider inherits the exile's target, and
/// the second mode's player slot below it is still announced. Both modes happen.
#[test]
fn gain_life_rider_does_not_hide_a_later_mode_slot() {
    let mut b = swords_board();
    let (spell, four) = (b.spell, b.four);
    let life_before = b.runner.state().players[1].life;
    let outcome = b
        .runner
        .cast(spell)
        .modes(&[0, 1])
        .target_object(four)
        .target_player(P0)
        .resolve();
    outcome.assert_zone(&[four], Zone::Exile);
    assert_eq!(
        outcome.state().players[1].life,
        life_before + 4,
        "its controller gains life equal to its power"
    );
    outcome.assert_hand_drawn(P0, 1);
}

/// CR 608.2b: the exile's target is bounced in response while the draw's player
/// target stays legal, so the spell resolves. The exile does nothing and the
/// life-gain rider needs information about the illegal target, so P1 gains
/// nothing; P0 still draws.
#[test]
fn modal_swords_first_target_bounced_rider_reads_nothing() {
    let mut b = swords_board();
    let (spell, four, unsummon) = (b.spell, b.four, b.unsummon);
    let life_before = b.runner.state().players[1].life;
    let p0_library_before = b.runner.state().players[0].library.len();
    b.runner
        .cast(spell)
        .modes(&[0, 1])
        .target_object(four)
        .target_player(P0)
        .commit();
    b.runner.cast(unsummon).target_object(four).resolve();
    assert_eq!(
        b.runner.state().objects[&four].zone,
        Zone::Hand,
        "reach guard: Unsummon resolved"
    );
    assert!(
        b.runner.state().stack.is_empty(),
        "reach guard: stack drained"
    );
    assert_eq!(
        b.runner.state().players[0].library.len(),
        p0_library_before - 1,
        "reach guard: the draw mode resolved for P0"
    );
    assert_eq!(
        b.runner.state().players[1].life,
        life_before,
        "CR 608.2b: no life from an illegal target's power"
    );
}

// ---------------------------------------------------------------------------
// W1 (review round 2): an inheriting rider's snapshot is not a second
// announcement. Printed Aboleth Spawn (Ward {2}) is the tapped target.
// ---------------------------------------------------------------------------

const ABOLETH_SPAWN: &str = "Flash\nWard {2}\nProbing Telepathy — Whenever a creature entering under an opponent's control causes a triggered ability of that creature to trigger, you may copy that ability. You may choose new targets for the copy.";

fn add_aboleth_spawn(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature(P1, name, 2, 3)
        .from_oracle_text_with_keywords(&["Flash", "Ward", "Probing Telepathy"], ABOLETH_SPAWN)
        .id()
}

fn becomes_target_count(events: &[GameEvent], object: ObjectId) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::BecomesTarget {
                    target: TargetRef::Object(o),
                    ..
                } if *o == object
            )
        })
        .count()
}

fn triggers_from(state: &GameState, source: ObjectId) -> Vec<ObjectId> {
    state
        .stack
        .iter()
        .filter(|e| {
            e.source_id == source && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|e| e.id)
        .collect()
}

fn displayed_target_count(state: &GameState, entry: ObjectId) -> usize {
    derive_views(state, None)
        .stack_entry_details
        .get(&entry)
        .map_or(0, |d| d.targets.len())
}

/// CR 115.1 + CR 115.10a + CR 702.21a: the Shuriken trigger targets the Spawn
/// ONCE — the gated rider only reads that announcement — so the Spawn becomes
/// the target once, Ward triggers once, one target is displayed, and paying
/// {2} once lets the trigger resolve (tap; 2 − 1 = +1). `decoy` adds a second
/// legal target so the target is chosen through the prompt (manual) rather than
/// auto-selected.
fn shuriken_against_ward(decoy: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Small Attacker", 1, 1).id();
    let shuriken = scenario
        .add_artifact_from_oracle(P0, "Conformer Shuriken", SHURIKEN)
        .with_subtypes(vec!["Equipment"])
        .id();
    let spawn = add_aboleth_spawn(&mut scenario, "Aboleth Spawn");
    if decoy {
        scenario.add_creature(P1, "Decoy", 1, 1);
    }
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&shuriken).unwrap().attached_to =
            Some(AttachTarget::Object(attacker));
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .attachments
            .push(shuriken);
    }

    let mut events: Vec<GameEvent> = Vec::new();
    runner.advance_to_combat();
    events.extend(
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("attack")
            .events,
    );
    let mut prompted = false;
    let mut ward_triggers: Vec<ObjectId> = Vec::new();
    let mut displayed: Option<usize> = None;
    let mut unless_prompts = 0;
    for _ in 0..60 {
        for id in triggers_from(runner.state(), spawn) {
            if !ward_triggers.contains(&id) {
                ward_triggers.push(id);
            }
        }
        if displayed.is_none() && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            if let Some(&entry) = triggers_from(runner.state(), attacker).first() {
                displayed = Some(displayed_target_count(runner.state(), entry));
            }
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                prompted = true;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(spawn)),
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::UnlessPayment { .. } => {
                unless_prompts += 1;
                // Exactly {2}, once, at the first Ward payment.
                if unless_prompts == 1 {
                    for _ in 0..2 {
                        let _ = runner.state_mut().add_mana_to_pool(
                            P0,
                            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                        );
                    }
                }
                GameAction::PayUnlessCost {
                    pay: unless_prompts == 1,
                }
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            _ => break,
        };
        events.extend(runner.act(action).expect("action accepted").events);
    }

    assert_eq!(
        prompted, decoy,
        "reach guard: manual vs automatic selection"
    );
    assert_eq!(
        becomes_target_count(&events, spawn),
        1,
        "CR 115.10a: the Spawn becomes the target once"
    );
    assert_eq!(ward_triggers.len(), 1, "CR 702.21a: Ward triggers once");
    assert_eq!(displayed, Some(1), "one displayed target on the trigger");
    assert_eq!(unless_prompts, 1, "one Ward payment prompt");
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: stack drained"
    );
    assert!(
        runner.state().objects[&spawn].tapped,
        "paying Ward once lets the trigger resolve"
    );
    assert_eq!(p1p1(&runner, attacker), 1);
}

#[test]
fn shuriken_target_with_ward_is_announced_once_automatic_selection() {
    shuriken_against_ward(false);
}

#[test]
fn shuriken_target_with_ward_is_announced_once_manual_selection() {
    shuriken_against_ward(true);
}

/// CR 115.10a + CR 702.21a: the #3864 life-gain rider ("Its controller gains
/// life equal to its power.") is the same inherited-snapshot shape: casting
/// Swords to Plowshares' instruction at a Ward creature announces it once.
#[test]
fn gain_life_rider_does_not_double_announce_a_ward_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swords = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Swords to Plowshares",
            true,
            "Exile target creature. Its controller gains life equal to its power.",
        )
        .id();
    let spawn = add_aboleth_spawn(&mut scenario, "Aboleth Spawn");
    let mut runner = scenario.build();
    let commit = runner.cast(swords).target_object(spawn).commit();
    let state = commit.state();
    let spell_entry = state
        .stack
        .iter()
        .find(|e| e.source_id == swords)
        .map(|e| e.id)
        .expect("reach guard: Swords is on the stack");
    assert_eq!(triggers_from(state, spawn).len(), 1, "Ward triggers once");
    assert_eq!(displayed_target_count(state, spell_entry), 1);
}

/// Control: two genuinely distinct instances of "target" keep their
/// multiplicity — the Ward creature and a second creature are both announced
/// (two displayed targets), and Ward triggers for its one announcement.
#[test]
fn two_printed_targets_still_announce_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Two Targets",
            true,
            "Tap target creature. Target creature gets +1/+0 until end of turn.",
        )
        .id();
    let first = add_aboleth_spawn(&mut scenario, "Spawn A");
    let second = scenario.add_creature(P1, "Plain Bear", 2, 2).id();
    let mut runner = scenario.build();
    let commit = runner.cast(spell).target_objects(&[first, second]).commit();
    let state = commit.state();
    let spell_entry = state
        .stack
        .iter()
        .find(|e| e.source_id == spell)
        .map(|e| e.id)
        .expect("reach guard: the spell is on the stack");
    assert_eq!(triggers_from(state, first).len(), 1);
    assert_eq!(displayed_target_count(state, spell_entry), 2);
}

// ---------------------------------------------------------------------------
// W2 (review round 3): target-count costs read ANNOUNCED targets. Strive
// ("costs {1} more for each target beyond the first") over the Swords modal:
// two targets are announced (the creature and the player); the life-gain
// rider's snapshot is not a third.
// ---------------------------------------------------------------------------

const STRIVE_LINE: &str = "This spell costs {1} more to cast for each target beyond the first.";

/// Casts the strive modal for {W} with both modes from a pool of {W} plus
/// `generic` colorless. Returns the mana left in P0's pool, the creature's
/// zone, P1's life change, and P0's draws.
fn cast_strive_modal(body: &str, generic: usize) -> (usize, Zone, i32, i64) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Creature A", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strive Modal", true, &format!("{STRIVE_LINE}\n{body}"))
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let mut pool = vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])];
    pool.extend(
        (0..generic).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])),
    );
    scenario.with_mana_pool(P0, pool);
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
    }
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&spell].strive_cost.is_some(),
        "reach guard: the strive surcharge parsed"
    );
    let life_before = runner.state().players[1].life;
    let outcome = runner
        .cast(spell)
        .modes(&[0, 1])
        .target_object(creature)
        .target_player(P0)
        .resolve();
    let state = outcome.state();
    (
        state.players[0].mana_pool.total(),
        state.objects[&creature].zone,
        state.players[1].life - life_before,
        outcome.hand_drawn(P0),
    )
}

const SWORDS_MODAL_BODY: &str = "Choose one or both —\n• Exile target creature. Its controller gains life equal to its power.\n• Target player draws a card.";
const PLAIN_MODAL_BODY: &str =
    "Choose one or both —\n• Exile target creature.\n• Target player draws a card.";

/// CR 601.2f + CR 115.10a: two announced targets cost {1} more — not {2}. With
/// exactly {W}{1} the cast succeeds and resolves; with {W}{2} one mana is left.
#[test]
fn strive_counts_announced_targets_not_the_life_gain_snapshot() {
    let (left, zone, life, drawn) = cast_strive_modal(SWORDS_MODAL_BODY, 1);
    assert_eq!(left, 0, "the {{1}} surcharge consumed the pool");
    assert_eq!(zone, Zone::Exile, "reach guard: mode 1 resolved");
    assert_eq!(life, 3, "its controller gains life equal to its power");
    assert_eq!(drawn, 1, "reach guard: mode 2 resolved");

    let (left, ..) = cast_strive_modal(SWORDS_MODAL_BODY, 2);
    assert_eq!(left, 1, "exactly {{1}} of surcharge was charged");
}

/// Control: the same modal without the life-gain rider also costs {1} more.
#[test]
fn strive_control_without_an_inheriting_rider() {
    let (left, zone, life, drawn) = cast_strive_modal(PLAIN_MODAL_BODY, 2);
    assert_eq!(left, 1, "exactly {{1}} of surcharge was charged");
    assert_eq!(zone, Zone::Exile);
    assert_eq!(life, 0);
    assert_eq!(drawn, 1);
}

// ---------------------------------------------------------------------------
// W3 (review round 4): the illegal-slot stamp, its `ahead` offset and the
// `ParentTargetSlot` reader all number DECLARED slots. The chain is "Exile
// target creature (A). Its controller gains life equal to its power (inherited
// snapshot of A). Tap target creature (B)." with a `ParentTargetSlot { 1 }`
// counter reader appended — raw holdings [A, A, B], declared slots [A, B].
// ---------------------------------------------------------------------------

const EXILE_GAIN_TAP: &str =
    "Exile target creature. Its controller gains life equal to its power. Tap target creature.";
const SHROUD: &str = "Target creature gains shroud until end of turn.";

/// How a target is made illegal in response.
#[derive(Clone, Copy, PartialEq)]
enum Response {
    /// Shroud on B: B stays on the battlefield but is an illegal target.
    ShroudB,
    /// Printed Unsummon on A: A changes zone. The Leg-2 board uses "target
    /// creature an opponent controls" for the exile with B under P0's control,
    /// so the exile of the departed A has no other candidate: an exile whose
    /// target became illegal currently falls back to a resolution-time pick
    /// (it exiles B, or prompts when A stays in place under shroud) — a
    /// pre-existing, separately filed issue that reproduces without any rider.
    UnsummonA,
}

const EXILE_OPP_GAIN_TAP: &str = "Exile target creature an opponent controls. Its controller gains life equal to its power. Tap target creature.";

/// Returns (A's zone, B tapped, B's +1/+1 counters, P1's life change, B's zone).
fn exile_gain_tap_with_slot_reader(response: Option<Response>) -> (Zone, bool, u32, i32, Zone) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let leg2 = response == Some(Response::UnsummonA);
    let a = scenario.add_creature(P1, "Creature A", 3, 3).id();
    let b = scenario
        .add_creature(if leg2 { P0 } else { P1 }, "Creature B", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Exile Tap",
            true,
            if leg2 {
                EXILE_OPP_GAIN_TAP
            } else {
                EXILE_GAIN_TAP
            },
        )
        .id();
    let veil = scenario
        .add_spell_to_hand_from_oracle(P0, "Veil", true, SHROUD)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[1].life;
    let _ = runner.cast(spell).target_objects(&[a, b]).commit();
    {
        let entry = runner
            .state_mut()
            .stack
            .iter_mut()
            .find(|e| e.source_id == spell)
            .expect("reach guard: the spell is on the stack");
        let mut node = entry.ability_mut().expect("spell ability");
        while node.sub_ability.is_some() {
            node = node.sub_ability.as_deref_mut().unwrap();
        }
        // The `ParentTargetSlot { 1 }` reader is appended synthetically: no
        // printed Oracle shape combines an inherited rider with a slot reader,
        // so this exercises the shared declared-slot numbering directly.
        node.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::ParentTargetSlot { index: 1 },
            },
            vec![],
            spell,
            P0,
        )));
    }
    let root = runner
        .state()
        .stack
        .iter()
        .find(|e| e.source_id == spell)
        .and_then(|e| e.ability())
        .expect("spell ability");
    assert_eq!(
        engine::game::ability_utils::flatten_targets_in_chain(root),
        vec![
            TargetRef::Object(a),
            TargetRef::Object(a),
            TargetRef::Object(b)
        ],
        "reach guard: the rider carries a snapshot of A"
    );
    assert_eq!(
        engine::game::ability_utils::declared_targets_in_chain(root),
        vec![TargetRef::Object(a), TargetRef::Object(b)],
    );
    match response {
        Some(Response::ShroudB) => {
            let _ = runner.cast(veil).target_object(b).commit();
        }
        Some(Response::UnsummonA) => {
            let _ = runner.cast(unsummon).target_object(a).commit();
        }
        None => {}
    }
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order");
            }
            _ => break,
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: stack drained"
    );
    let state = runner.state();
    (
        state.objects[&a].zone,
        state.objects[&b].tapped,
        state.objects[&b]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        state.players[1].life - life_before,
        state.objects[&b].zone,
    )
}

/// Control: both legal — A exiled (P1 gains 3), B tapped, the slot-1 reader
/// counts B.
#[test]
fn slot_reader_control_reads_the_second_declared_target() {
    assert_eq!(
        exile_gain_tap_with_slot_reader(None),
        (Zone::Exile, true, 1, 3, Zone::Battlefield)
    );
}

/// CR 608.2b: B (declared slot 1) becomes illegal; the tap skips it AND the
/// slot-1 reader gets nothing — the stamp and the reader number the same slot.
#[test]
fn illegal_second_target_is_not_read_through_its_slot() {
    let (a_zone, b_tapped, b_counters, _, _) =
        exile_gain_tap_with_slot_reader(Some(Response::ShroudB));
    assert_eq!(a_zone, Zone::Exile, "reach guard: the legal exile resolved");
    assert!(!b_tapped, "the tap skips the illegal B");
    assert_eq!(b_counters, 0, "CR 608.2b: no counter on the illegal B");
}

/// CR 608.2b: A (declared slot 0) leaves in response (printed Unsummon) while B
/// stays legal: A is not exiled and its controller gains nothing, B is tapped,
/// and the slot-1 reader still finds B — neither the illegal first target nor
/// the rider's carried copy of it shifts slot 1.
#[test]
fn illegal_first_target_leaves_the_second_slot_readable() {
    let (a_zone, b_tapped, b_counters, life, b_zone) =
        exile_gain_tap_with_slot_reader(Some(Response::UnsummonA));
    assert_eq!(
        a_zone,
        Zone::Hand,
        "reach guard: Unsummon resolved; A is not exiled"
    );
    assert_eq!(life, 0, "CR 608.2b: no life from the illegal A's power");
    assert_eq!(b_zone, Zone::Battlefield, "reach guard: B was not exiled");
    assert!(b_tapped, "reach guard: the legal B was tapped");
    assert_eq!(b_counters, 1, "slot 1 is still B");
}

// ---------------------------------------------------------------------------
// CodeRabbit CR1 (#9434): the validation restamp uses the same inherited-rider
// criterion as every declared-slot site. A deferred parent (Surveil) over a
// life-gain anaphor rider does NOT hand its rider a carried snapshot ? that
// rider holds a target of its own, which validation must keep.
// ---------------------------------------------------------------------------

/// Which head the life-gain anaphor rider hangs under.
#[derive(Clone, Copy, PartialEq)]
enum RiderHead {
    /// "Surveil 1": a deferred effect with no target of its own.
    Surveil,
    /// "Exile target creature": the rider inherits the exile's target.
    Exile,
}

/// Returns (the rider's targets after validation, Surveil prompts, P1's life
/// change, the victim's zone).
fn life_gain_rider_under(head: RiderHead) -> (Vec<TargetRef>, usize, i32, Zone) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 3, 3).id();
    let source = scenario
        .add_artifact_from_oracle(P0, "Rider Source", "{T}: You gain 1 life.")
        .id();
    for i in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let mut runner = scenario.build();

    let head_effect: Effect = serde_json::from_value(match head {
        RiderHead::Surveil => serde_json::json!({
            "type": "Surveil",
            "count": {"type": "Fixed", "value": 1},
            "target": {"type": "Controller"}
        }),
        RiderHead::Exile => serde_json::json!({
            "type": "ChangeZone",
            "origin": null,
            "destination": "Exile",
            "target": {"type": "Typed", "type_filters": ["Creature"], "controller": null, "properties": []},
            "owner_library": false,
            "enter_transformed": false,
            "enter_tapped": false,
            "enters_attacking": false
        }),
    })
    .expect("head effect");
    let rider_effect: Effect = serde_json::from_value(serde_json::json!({
        "type": "GainLife",
        "amount": {"type": "Ref", "qty": {"type": "Power", "scope": {"type": "Target"}}},
        "player": {"type": "ParentTargetController"}
    }))
    .expect("life-gain anaphor rider");
    let head_targets = match head {
        RiderHead::Surveil => vec![],
        RiderHead::Exile => vec![TargetRef::Object(victim)],
    };
    let mut chain = ResolvedAbility::new(head_effect, head_targets, source, P0);
    chain.sub_ability = Some(Box::new(ResolvedAbility::new(
        rider_effect,
        vec![TargetRef::Object(victim)],
        source,
        P0,
    )));

    let validated = validate_targets_in_chain(runner.state(), &chain);
    let rider_targets = validated
        .sub_ability
        .as_ref()
        .map(|rider| rider.targets.clone())
        .unwrap_or_default();

    let life_before = runner.state().players[1].life;
    {
        let state = runner.state_mut();
        state.stack.push_back(StackEntry {
            id: source,
            source_id: source,
            controller: P0,
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(chain),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Rider Source".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let mut surveil_prompts = 0;
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::SurveilChoice { cards, .. } => {
                surveil_prompts += 1;
                GameAction::SelectCards { cards }
            }
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the stack drained"
    );
    let life = runner.state().players[1].life - life_before;
    let zone = runner.state().objects[&victim].zone;
    (rider_targets, surveil_prompts, life, zone)
}

/// CR 601.2c + CR 608.2b: under a deferred Surveil head the rider's target is its
/// own selection, not a snapshot of the head's (absent) target. Validation keeps
/// it, Surveil happens, and the victim's controller gains 3.
#[test]
fn deferred_parent_rider_keeps_its_own_target() {
    let (rider_targets, surveil_prompts, life, zone) = life_gain_rider_under(RiderHead::Surveil);
    assert_eq!(
        zone,
        Zone::Battlefield,
        "reach guard: nothing exiled the victim"
    );
    assert_eq!(
        rider_targets.len(),
        1,
        "the rider keeps its selected target through validation"
    );
    assert_eq!(
        surveil_prompts, 1,
        "Surveil 1 resolves: the ability does not fizzle"
    );
    assert_eq!(life, 3, "P1 gains life equal to the victim's power");
}

/// Control: under an exile head the rider inherits the exile's target, and the
/// restamp re-derives it from the validated parent.
#[test]
fn inherited_rider_under_an_exile_head_resolves() {
    let (rider_targets, surveil_prompts, life, zone) = life_gain_rider_under(RiderHead::Exile);
    assert_eq!(surveil_prompts, 0);
    assert_eq!(zone, Zone::Exile, "reach guard: the exile resolved");
    assert_eq!(rider_targets.len(), 1, "the restamp keeps the legal target");
    assert_eq!(
        life, 3,
        "P1 gains life equal to the exiled creature's power"
    );
}
