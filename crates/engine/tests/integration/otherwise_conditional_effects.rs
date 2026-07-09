//! "If <C>, A. Otherwise, B." per-clause condition recognition — end-to-end.
//!
//! CR 608.2c + CR 603.4: a card that reads "If <condition>, <effect A>.
//! Otherwise, <effect B>." attaches B to A as A's `else_ability`. The "Otherwise"
//! plumbing keys off the FIRST preceding clause whose `condition.is_some()`. The
//! cards below previously dropped the "If <C>, A" clause's condition (it parsed
//! to `condition: null`), so the `else` branch found no anchor and the parser
//! emitted `Effect::Unimplemented { name: "otherwise" }` that ran B
//! unconditionally. The fix is purely per-clause condition recognition:
//!   - anaphoric/self permanent status ("if it's tapped" / "if it's suspected" /
//!     "if ~ is suspected") → Target/Source `MatchesFilter` (CR 611.2b, CR 701.60b);
//!   - negated saddled designation ("if ~ isn't saddled") → Not(SourceMatchesFilter)
//!     against `FilterProp::IsSaddled` (CR 702.171b);
//!   - "they lost life this turn" → `LifeLostThisTurn { Target }` (CR 119.3, CR 115.1);
//!   - "you don't control a Snail" → Not(IsPresent) (already supported).
//!
//! Each test drives BOTH branches through the real apply() pipeline so reverting
//! the condition recognition (which makes the `else` branch fire unconditionally)
//! fails the "if" branch's assertion.

use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// ---------------------------------------------------------------------------
// Shared driving helpers
// ---------------------------------------------------------------------------

/// Pass priority repeatedly until the stack is empty or a non-Priority prompt
/// appears, returning the prompt the pipeline halted on.
fn drain_priority(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Answer a single-object target prompt (spell or trigger) by selecting `victim`.
fn select_object_target(runner: &mut engine::game::scenario::GameRunner, victim: ObjectId) {
    let slots = match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. }
        | WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots,
        other => panic!("expected a target selection, got {other:?}"),
    };
    let choice = slots[0]
        .legal_targets
        .iter()
        .find(|t| matches!(t, TargetRef::Object(id) if *id == victim))
        .cloned()
        .expect("victim must be a legal target");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![choice],
        })
        .expect("selecting the trigger target must succeed");
}

/// Add `count` mana of `ty` to P0's pool for deterministic payment.
fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn counters_on(runner: &engine::game::scenario::GameRunner, id: ObjectId, ct: CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&ct).copied())
        .unwrap_or(0)
}

fn is_suspected(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.is_suspected)
        .unwrap_or(false)
}

fn is_tapped(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.tapped)
        .unwrap_or(false)
}

fn zone_of(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.zone)
        .expect("object exists")
}

// ===========================================================================
// Repeat Offender — activated ability, self target
// "{2}{B}: If this creature is suspected, put a +1/+1 counter on it.
//  Otherwise, suspect it."
// ===========================================================================

const REPEAT_OFFENDER: &str =
    "{2}{B}: If this creature is suspected, put a +1/+1 counter on it. Otherwise, suspect it.";

/// "if" branch: a SUSPECTED Repeat Offender gets a +1/+1 counter — and is NOT
/// re-suspected by the else branch (suspect is a no-op on an already-suspected
/// permanent, so the discriminator is the +1/+1 counter the else branch would
/// never add). Revert → the else "suspect it" runs unconditionally and NO
/// counter is placed: `assert_eq!(counters, 1)` flips to 0.
#[test]
fn repeat_offender_suspected_gets_counter() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let offender = scenario
        .add_creature_from_oracle(P0, "Repeat Offender", 2, 2, REPEAT_OFFENDER)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&offender)
        .unwrap()
        .is_suspected = true;
    add_mana(&mut runner, ManaType::Black, 3);

    runner
        .act(GameAction::ActivateAbility {
            source_id: offender,
            ability_index: 0,
        })
        .expect("activating Repeat Offender must succeed");
    drain_priority(&mut runner);

    assert_eq!(
        counters_on(&runner, offender, CounterType::Plus1Plus1),
        1,
        "suspected Repeat Offender must take the +1/+1 counter (the 'if' branch)"
    );
}

/// Negative case for the condition gate AND the "otherwise" effect: an
/// UN-suspected Repeat Offender must NOT take a +1/+1 counter (the "if" branch
/// must not run) and MUST instead become suspected (the "otherwise" branch's
/// "suspect it" acts on the source).
///
/// Two revert discriminators:
///   - `counters == 0`: on revert of FIX 1's condition gate, the un-gated
///     PutCounter runs and an un-suspected source DOES gain a counter → flips to 1.
///   - `is_suspected`: on revert of FIX 2 (the self-targeting else-lowering /
///     `Suspect{SelfRef}` source binding), "suspect it" lowers "it" to
///     `ParentTarget` and resolves against the empty announced-target list, so
///     the source stays un-suspected → flips to false.
#[test]
fn repeat_offender_unsuspected_is_suspected_not_countered() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let offender = scenario
        .add_creature_from_oracle(P0, "Repeat Offender", 2, 2, REPEAT_OFFENDER)
        .id();
    let mut runner = scenario.build();
    assert!(!is_suspected(&runner, offender));
    add_mana(&mut runner, ManaType::Black, 3);

    runner
        .act(GameAction::ActivateAbility {
            source_id: offender,
            ability_index: 0,
        })
        .expect("activating Repeat Offender must succeed");
    drain_priority(&mut runner);

    assert_eq!(
        counters_on(&runner, offender, CounterType::Plus1Plus1),
        0,
        "the 'if' branch must NOT run for an un-suspected source — no counter \
         (revert makes the un-gated PutCounter run and this flips to 1)"
    );
    assert!(
        is_suspected(&runner, offender),
        "the 'otherwise' branch ('suspect it') must suspect the source itself — \
         a self-targeting activated ability with no announced target binds 'it' to \
         the source (revert of the else-lowering makes 'suspect it' a no-op against \
         an empty target list and this flips to false)"
    );
}

// ===========================================================================
// Agrus Kos, Spirit of Justice — attack trigger, chosen target
// "Whenever Agrus Kos enters or attacks, choose up to one target creature.
//  If it's suspected, exile it. Otherwise, suspect it."
// ===========================================================================

const AGRUS_KOS: &str = "Double strike, vigilance\nWhenever Agrus Kos enters or attacks, choose up to one target creature. If it's suspected, exile it. Otherwise, suspect it.";

/// "if" branch: a SUSPECTED target creature is exiled. Revert → "suspect it"
/// runs unconditionally, the suspected target stays on the battlefield (suspect
/// is a no-op on it) and is NOT exiled: `assert_eq!(zone, Exile)` flips.
#[test]
fn agrus_kos_suspected_target_is_exiled() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let agrus = scenario
        .add_creature_from_oracle(P0, "Agrus Kos, Spirit of Justice", 3, 3, AGRUS_KOS)
        .id();
    let victim = scenario.add_creature(P1, "Suspect Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .is_suspected = true;

    fire_attack_trigger(&mut runner, agrus, victim);

    assert_eq!(
        zone_of(&runner, victim),
        Zone::Exile,
        "a suspected target must be exiled (the 'if' branch)"
    );
}

/// "otherwise" branch: a NON-suspected target becomes suspected and stays on the
/// battlefield (not exiled).
#[test]
fn agrus_kos_unsuspected_target_is_suspected() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let agrus = scenario
        .add_creature_from_oracle(P0, "Agrus Kos, Spirit of Justice", 3, 3, AGRUS_KOS)
        .id();
    let victim = scenario.add_creature(P1, "Plain Bear", 2, 2).id();
    let mut runner = scenario.build();
    assert!(!is_suspected(&runner, victim));

    fire_attack_trigger(&mut runner, agrus, victim);

    assert_eq!(
        zone_of(&runner, victim),
        Zone::Battlefield,
        "a non-suspected target must NOT be exiled (the 'if' branch must not run)"
    );
    assert!(
        is_suspected(&runner, victim),
        "a non-suspected target must become suspected (the 'otherwise' branch)"
    );
}

/// Declare `attacker` attacking P1, answer the choose-target prompt with
/// `victim`, and drain the trigger off the stack.
fn fire_attack_trigger(
    runner: &mut engine::game::scenario::GameRunner,
    attacker: ObjectId,
    victim: ObjectId,
) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declaring the attack must succeed");
    // The attack trigger needs a target chosen as it goes on the stack.
    drive_to_target_then_resolve(runner, victim);
}

/// Drive the pipeline answering an object target with `victim`, then drain.
fn drive_to_target_then_resolve(runner: &mut engine::game::scenario::GameRunner, victim: ObjectId) {
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                select_object_target(runner, victim);
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                // Single trigger; resolve in any order.
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
}

// ===========================================================================
// Shackle Slinger — cast-second-spell trigger, chosen target
// "Whenever you cast your second spell each turn, choose target creature an
//  opponent controls. If it's tapped, put a stun counter on it. Otherwise, tap it."
// ===========================================================================

const SHACKLE_SLINGER: &str = "Whenever you cast your second spell each turn, choose target creature an opponent controls. If it's tapped, put a stun counter on it. Otherwise, tap it.";

/// "if" branch: a TAPPED target opponent creature gets a stun counter (and is
/// already tapped). Revert → "tap it" runs unconditionally, no stun counter is
/// placed: `assert_eq!(stun, 1)` flips to 0.
#[test]
fn shackle_slinger_tapped_target_gets_stun_counter() {
    let (mut runner, victim) = shackle_slinger_setup(/* victim_tapped */ true);
    cast_two_spells_then_resolve(&mut runner, victim);

    assert_eq!(
        counters_on(&runner, victim, CounterType::Stun),
        1,
        "a tapped target must get a stun counter (the 'if' branch)"
    );
}

/// "otherwise" branch: an UNTAPPED target becomes tapped and gets NO stun counter.
#[test]
fn shackle_slinger_untapped_target_is_tapped() {
    let (mut runner, victim) = shackle_slinger_setup(/* victim_tapped */ false);
    cast_two_spells_then_resolve(&mut runner, victim);

    assert!(
        is_tapped(&runner, victim),
        "an untapped target must become tapped (the 'otherwise' branch)"
    );
    assert_eq!(
        counters_on(&runner, victim, CounterType::Stun),
        0,
        "an untapped target must NOT get a stun counter (the 'if' branch must not run)"
    );
}

fn shackle_slinger_setup(victim_tapped: bool) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 19);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Shackle Slinger", 2, 2, SHACKLE_SLINGER);
    let victim = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    // Two free spells in P0's hand to satisfy the second-spell trigger.
    let _s1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Cantrip One", true, "Draw a card.")
        .id();
    let _s2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Cantrip Two", true, "Draw a card.")
        .id();
    scenario.with_library_top(P0, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    let mut runner = scenario.build();
    if victim_tapped {
        runner.state_mut().objects.get_mut(&victim).unwrap().tapped = true;
    }
    (runner, victim)
}

/// Cast both cantrips (the second triggers Shackle Slinger), answer the trigger
/// target, and drain.
fn cast_two_spells_then_resolve(runner: &mut engine::game::scenario::GameRunner, victim: ObjectId) {
    let spells: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand && o.owner == P0)
        .map(|o| o.id)
        .collect();
    assert_eq!(spells.len(), 2, "expected two castable cantrips in hand");
    for spell in spells {
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting cantrip must succeed");
        drive_to_target_then_resolve(runner, victim);
    }
}

// ===========================================================================
// Caustic Bronco — attack trigger, source-saddled gate, no chosen target
// "Whenever this creature attacks, reveal the top card of your library and put
//  it into your hand. You lose life equal to that card's mana value if this
//  creature isn't saddled. Otherwise, each opponent loses that much life."
// ===========================================================================

const CAUSTIC_BRONCO: &str = "Whenever this creature attacks, reveal the top card of your library and put it into your hand. You lose life equal to that card's mana value if this creature isn't saddled. Otherwise, each opponent loses that much life.";

/// "if" branch (NOT saddled): the controller (P0) loses life equal to the
/// revealed card's mana value; opponents are untouched. Revert → "each opponent
/// loses that much life" runs unconditionally: P0's life delta flips to 0 and P1
/// loses the life instead.
#[test]
fn caustic_bronco_not_saddled_controller_loses_life() {
    let (mut runner, bronco, mv) = caustic_bronco_setup(/* saddled */ false);
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    fire_caustic_bronco_attack(&mut runner, bronco);

    assert_eq!(
        runner.life(P0),
        p0_before - mv,
        "an un-saddled Bronco makes its controller lose life=MV (the 'if' branch)"
    );
    assert_eq!(
        runner.life(P1),
        p1_before,
        "the opponent must not lose life when Bronco is un-saddled"
    );
}

/// Negative case for the saddled gate AND the "otherwise" amount: when Bronco IS
/// saddled, the controller must NOT lose life (the "if" branch must not run) and
/// each opponent MUST lose life equal to the revealed card's mana value (the
/// "otherwise" branch's "that much life").
///
/// Two revert discriminators:
///   - `P0 == before`: on revert of FIX 1's saddled condition gate, the un-gated
///     "you lose life" runs and P0 loses MV → flips.
///   - `P1 == before - mv`: on revert of FIX 3 (binding the else's "that much" to
///     the revealed card's stable mana value), "that much" lowers to
///     `EventContextAmount`, which reads the SKIPPED "you lose life" instruction's
///     amount (0) in the saddled branch, so P1 loses 0 → flips.
#[test]
fn caustic_bronco_saddled_opponents_lose_revealed_mana_value() {
    let (mut runner, bronco, mv) = caustic_bronco_setup(/* saddled */ true);
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    fire_caustic_bronco_attack(&mut runner, bronco);

    assert_eq!(
        runner.life(P0),
        p0_before,
        "the controller must not lose life when Bronco is saddled — the 'if' branch \
         must not run (revert makes the un-gated 'you lose life' run and this flips)"
    );
    assert_eq!(
        runner.life(P1),
        p1_before - mv,
        "the opponent must lose life equal to the revealed card's mana value (the \
         'otherwise' branch's 'that much life'); revert of the stable-amount binding \
         makes 'that much' read the skipped instruction's amount (0) and this flips"
    );
}

/// Returns `(runner, bronco_id, revealed_mana_value)`.
fn caustic_bronco_setup(saddled: bool) -> (engine::game::scenario::GameRunner, ObjectId, i32) {
    let mut scenario = GameScenario::new_n_player(2, 23);
    scenario.at_phase(Phase::PreCombatMain);
    let bronco = scenario
        .add_creature_from_oracle(P0, "Caustic Bronco", 2, 2, CAUSTIC_BRONCO)
        .id();
    // Top of library: a 3-MV spell so the revealed card's mana value is a fixed,
    // non-trivial amount (distinguishes "lost MV" from "lost 0/1").
    let top = scenario
        .add_spell_to_library_top(P0, "Top Reveal", false)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&top).unwrap().mana_cost = ManaCost::generic(3);
    if saddled {
        runner
            .state_mut()
            .objects
            .get_mut(&bronco)
            .unwrap()
            .is_saddled = true;
    }
    (runner, bronco, 3)
}

fn fire_caustic_bronco_attack(runner: &mut engine::game::scenario::GameRunner, bronco: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bronco, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declaring Bronco's attack must succeed");
    drain_priority(runner);
}

// ===========================================================================
// Wick, the Whorled Mind — ETB trigger, "you don't control a Snail" gate
// "Whenever Wick or another Rat you control enters, create a 1/1 black Snail
//  creature token if you don't control a Snail. Otherwise, put a +1/+1 counter
//  on a Snail you control."
// ===========================================================================

const WICK: &str = "Whenever Wick or another Rat you control enters, create a 1/1 black Snail creature token if you don't control a Snail. Otherwise, put a +1/+1 counter on a Snail you control.";

#[test]
fn wick_no_snail_creates_token() {
    let mut scenario = GameScenario::new_n_player(2, 29);
    scenario.at_phase(Phase::PreCombatMain);
    let wick = {
        let mut b =
            scenario.add_creature_to_hand_from_oracle(P0, "Wick, the Whorled Mind", 2, 2, WICK);
        b.as_legendary();
        b.with_subtypes(vec!["Rat"]);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();
    let snails_before = count_snails(&runner);

    cast_and_resolve_etb(&mut runner, wick);

    assert!(
        count_snails(&runner) > snails_before,
        "with no Snail controlled, Wick's ETB must create a Snail token (the 'if' branch)"
    );
}

#[test]
fn wick_with_snail_pumps_existing_snail() {
    let mut scenario = GameScenario::new_n_player(2, 29);
    scenario.at_phase(Phase::PreCombatMain);
    let existing_snail = {
        let mut b = scenario.add_creature(P0, "Garden Snail", 1, 1);
        b.with_subtypes(vec!["Snail"]);
        b.id()
    };
    let wick = {
        let mut b =
            scenario.add_creature_to_hand_from_oracle(P0, "Wick, the Whorled Mind", 2, 2, WICK);
        b.as_legendary();
        b.with_subtypes(vec!["Rat"]);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();
    let snails_before = count_snails(&runner);

    cast_and_resolve_etb(&mut runner, wick);

    assert_eq!(
        count_snails(&runner),
        snails_before,
        "with a Snail already controlled, no new Snail token is created (the 'if' branch must not run)"
    );
    assert_eq!(
        counters_on(&runner, existing_snail, CounterType::Plus1Plus1),
        1,
        "the existing Snail must gain a +1/+1 counter (the 'otherwise' branch)"
    );
}

#[test]
fn wick_with_two_snails_pumps_chosen_snail() {
    let mut scenario = GameScenario::new_n_player(2, 29);
    scenario.at_phase(Phase::PreCombatMain);
    let snail_a = {
        let mut b = scenario.add_creature(P0, "Snail A", 1, 1);
        b.with_subtypes(vec!["Snail"]);
        b.id()
    };
    let snail_b = {
        let mut b = scenario.add_creature(P0, "Snail B", 1, 1);
        b.with_subtypes(vec!["Snail"]);
        b.id()
    };
    let wick = {
        let mut b =
            scenario.add_creature_to_hand_from_oracle(P0, "Wick, the Whorled Mind", 2, 2, WICK);
        b.as_legendary();
        b.with_subtypes(vec!["Rat"]);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();
    let snails_before = count_snails(&runner);

    cast_and_resolve_etb(&mut runner, wick);

    assert_eq!(
        count_snails(&runner),
        snails_before,
        "with Snails already controlled, no new Snail token is created"
    );
    let counters_a = counters_on(&runner, snail_a, CounterType::Plus1Plus1);
    let counters_b = counters_on(&runner, snail_b, CounterType::Plus1Plus1);
    assert_eq!(
        counters_a + counters_b,
        1,
        "exactly one controlled Snail must receive the +1/+1 counter"
    );
    assert!(
        counters_a == 1 || counters_b == 1,
        "the chosen Snail must receive the counter (a={counters_a}, b={counters_b})"
    );
}

fn count_snails(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Snail"))
        })
        .count()
}

/// Cast a free creature and resolve it + its ETB trigger, answering any object
/// target (Wick's "+1/+1 on a Snail you control" picks the first legal Snail when
/// a choice is required).
fn cast_and_resolve_etb(runner: &mut engine::game::scenario::GameRunner, creature: ObjectId) {
    let card_id = runner.state().objects[&creature].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: creature,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the creature must succeed");
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::TargetSelection { target_slots, .. }
            | WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                let choice = target_slots[0]
                    .legal_targets
                    .first()
                    .cloned()
                    .expect("a legal target must exist");
                if runner
                    .act(GameAction::SelectTargets {
                        targets: vec![choice],
                    })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                let pick = cards.first().copied().expect("a legal Snail must exist");
                if runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
}

// ===========================================================================
// Thought-Stalker Warlock — ETB trigger, "they lost life this turn" gate
// "When this creature enters, choose target opponent. If they lost life this
//  turn, they reveal their hand, you choose a nonland card from it, and they
//  discard that card. Otherwise, they discard a card."
// ===========================================================================

const THOUGHT_STALKER: &str = "Menace\nWhen this creature enters, choose target opponent. If they lost life this turn, they reveal their hand, you choose a nonland card from it, and they discard that card. Otherwise, they discard a card.";

/// "if" branch: the target opponent LOST life this turn → the controller-chosen
/// discard fires (a `RevealChoice` / controller selection appears). Negative
/// branch below covers the un-lost-life case. The discriminator that flips on
/// revert is whether the controller (P0) gets to CHOOSE which card the opponent
/// discards: the "if" branch routes through a controller choice; the "otherwise"
/// branch is a plain self-discard with no controller choice.
#[test]
fn thought_stalker_opponent_lost_life_controller_chooses_discard() {
    let saw_controller_choice = run_thought_stalker(/* opp_lost_life */ true);
    assert!(
        saw_controller_choice,
        "when the target opponent lost life this turn, the controller must choose the \
         discarded nonland card (the 'if' branch)"
    );
}

/// "otherwise" branch: the target opponent did NOT lose life → a plain discard
/// with no controller choice.
#[test]
fn thought_stalker_opponent_no_life_loss_plain_discard() {
    let saw_controller_choice = run_thought_stalker(/* opp_lost_life */ false);
    assert!(
        !saw_controller_choice,
        "when the target opponent did not lose life, the controller must NOT choose \
         the discard (the 'if' branch must not run)"
    );
}

/// Returns true if the controller (P0) was prompted to choose a card from the
/// opponent's hand during resolution (the "if" branch's signature behavior).
fn run_thought_stalker(opp_lost_life: bool) -> bool {
    let mut scenario = GameScenario::new_n_player(2, 31);
    scenario.at_phase(Phase::PreCombatMain);
    // Opponent has cards to reveal/discard (a nonland card so the controller-
    // choice branch has something to pick).
    scenario.with_cards_in_hand(P1, &["Opp Spell A", "Opp Spell B"]);
    let stalker = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Thought-Stalker Warlock",
            2,
            2,
            THOUGHT_STALKER,
        );
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();
    if opp_lost_life {
        runner.state_mut().players[1].life_lost_this_turn = 3;
    }

    let card_id = runner.state().objects[&stalker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: stalker,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Thought-Stalker must succeed");

    let mut saw_controller_choice = false;
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::TargetSelection { target_slots, .. }
            | WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                // Target the opponent (P1).
                let choice = target_slots[0]
                    .legal_targets
                    .iter()
                    .find(|t| matches!(t, TargetRef::Player(p) if *p == P1))
                    .cloned()
                    .expect("the opponent must be a legal target");
                if runner
                    .act(GameAction::SelectTargets {
                        targets: vec![choice],
                    })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            // The "if" branch routes the opponent's nonland-card discard through a
            // CONTROLLER (P0) selection of a card from the revealed opponent hand;
            // a card-selection prompt directed at P0 is the signature of the "if"
            // branch having fired. The "otherwise" branch only ever prompts P1
            // (its own discard), so this stays false on revert.
            WaitingFor::RevealChoice { player, cards, .. } => {
                if player == P0 {
                    saw_controller_choice = true;
                }
                let pick: Vec<ObjectId> = cards.first().cloned().into_iter().collect();
                if runner.act(GameAction::SelectCards { cards: pick }).is_err() {
                    break;
                }
            }
            WaitingFor::DiscardChoice { player, cards, .. } => {
                if player == P0 {
                    saw_controller_choice = true;
                }
                let pick: Vec<ObjectId> = cards.first().cloned().into_iter().collect();
                if runner.act(GameAction::SelectCards { cards: pick }).is_err() {
                    break;
                }
            }
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                if player == P0 {
                    saw_controller_choice = true;
                }
                let pick: Vec<ObjectId> = cards.first().cloned().into_iter().collect();
                if runner.act(GameAction::SelectCards { cards: pick }).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    saw_controller_choice
}
