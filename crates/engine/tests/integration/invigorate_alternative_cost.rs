//! Invigorate — "If you control a Forest, rather than pay this spell's mana cost,
//! you may have an opponent gain 3 life." (CR 118.9 swapped clause order; the
//! life gain is an effect performed as a cost, CR 118.3 + CR 119.3, whose
//! recipient the caster chooses, CR 115.10a).
//!
//! Every fixture Oracle text below is verbatim from the local Scryfall-derived
//! dataset, except "Lifegain Negator", which is only the replacement clause the
//! engine's own parser tests pin (not a real card's full text).
//!
//! All tests drive the real cast pipeline through public APIs: `GameAction`s via
//! `GameRunner::act`, recording whether the `OptionalCostChoice` offer appeared.

use engine::ai_support::{
    classify_payment_continuation, legal_actions, witness_payment_continuations,
    PaymentContinuationBatchStatus, PaymentContinuationRoot, PaymentContinuationState,
};
use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const INVIGORATE: &str = "If you control a Forest, rather than pay this spell's mana cost, you may have an opponent gain 3 life.\nTarget creature gets +4/+4 until end of turn.";
const EVERLASTING_TORMENT: &str = "Players can't gain life.\nDamage can't be prevented.\nAll damage is dealt as though its source had wither. (A source with wither deals damage to creatures in the form of -1/-1 counters.)";
const DOCTOR_STRANGE: &str = "Lifelink\nIf you would gain life, you gain twice that much life instead.\nAt the beginning of each combat, if you have at least 10 life more than your starting life total, creatures you control get +2/+2 and gain vigilance until end of turn.";
const PEST_RESCUER: &str = "At the beginning of each upkeep, if you don't control a Pest creature token, create a 1/1 black and green Pest creature token with \"When this token dies, you gain 1 life.\"\nIf you would gain life, you gain that much life plus 1 instead.";
const ARCHFIEND_OF_DESPAIR: &str = "Flying\nYour opponents can't gain life.\nAt the beginning of each end step, each opponent loses life equal to the life that player lost this turn. (Damage causes loss of life.)";
/// Sulfuric Vortex's replacement CLAUSE only (see module doc).
const LIFEGAIN_NEGATOR: &str = "If a player would gain life, that player gains no life instead.";

struct Setup {
    scenario: GameScenario,
    inv: ObjectId,
    bear: ObjectId,
}

/// Invigorate ({2}{G} set explicitly; the dataset has no mana-cost field) in
/// P0's hand and a 2/2 target for P0.
fn setup(mut scenario: GameScenario) -> Setup {
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Invigorate", true, INVIGORATE);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Green],
    });
    let inv = builder.id();
    Setup {
        scenario,
        inv,
        bear,
    }
}

/// {G}{C}{C} floating for P0 — enough to cast Invigorate for its printed cost.
fn printed_cost_pool(scenario: &mut GameScenario, source: ObjectId) {
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, source, false, vec![]),
            ManaUnit::new(ManaType::Colorless, source, false, vec![]),
            ManaUnit::new(ManaType::Colorless, source, false, vec![]),
        ],
    );
}

#[derive(Debug, Default)]
struct CastTrace {
    saw_optional_cost: bool,
    replacement_prompt_player: Option<PlayerId>,
}

/// Cast Invigorate targeting `target`, answering the alternative-cost offer with
/// `pay_alt` and a replacement-ordering prompt with the candidate whose source
/// is `repl_pick`, then pass priority until the stack is empty. Panics on any
/// unexpected prompt.
fn drive_cast(
    runner: &mut GameRunner,
    inv: ObjectId,
    target: ObjectId,
    pay_alt: bool,
    repl_pick: Option<ObjectId>,
) -> CastTrace {
    let mut trace = CastTrace::default();
    let card_id = runner.state().objects[&inv].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: inv,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target)],
                    })
                    .expect("target selection should succeed");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                trace.saw_optional_cost = true;
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_alt })
                    .expect("optional cost decision should succeed");
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                trace.replacement_prompt_player = Some(player);
                let pick = repl_pick.expect("unexpected replacement prompt");
                let index = candidates
                    .iter()
                    .position(|c| c.source_id == pick)
                    .unwrap_or_else(|| panic!("no candidate from {pick:?}: {candidates:?}"));
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("replacement choice should succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment should auto-finalize");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return trace,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should succeed");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!(
        "cast pipeline did not finish; last waiting_for = {:?}",
        runner.state().waiting_for
    );
}

fn power(runner: &GameRunner, id: ObjectId) -> Option<i32> {
    runner.state().objects[&id].power
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

/// V4 — CR 118.9 + CR 118.3 + CR 119.3: paying the alternative cost makes the
/// opponent gain 3 life, spends no mana, and the pump still applies.
#[test]
fn v4_paying_alt_cost_opponent_gains_three_no_mana_spent() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (runner.life(P0), runner.life(P1));

    let trace = drive_cast(&mut runner, inv, bear, true, None);

    assert!(trace.saw_optional_cost, "alternative cost must be offered");
    assert_eq!(runner.life(P1), p1_before + 3, "opponent gains 3 life");
    assert_eq!(runner.life(P0), p0_before, "caster's life unchanged");
    assert!(!tapped(&runner, forest), "no mana spent");
    assert_eq!(power(&runner, bear), Some(6), "pump resolved");
}

/// V5 — declining pays the printed cost and nobody gains life.
#[test]
fn v5_declining_pays_printed_cost_without_life_gain() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    let lands = [
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
    ];
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);

    let trace = drive_cast(&mut runner, inv, bear, false, None);

    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before, "no life gain when declined");
    assert!(
        lands.iter().all(|&l| tapped(&runner, l)),
        "printed {{2}}{{G}} paid"
    );
    assert_eq!(power(&runner, bear), Some(6));
}

/// V6 — CR 118.9 + CR 601.3d: without a Forest the alternative cost is not
/// offered. Paired positive with a Forest in the same test.
#[test]
fn v6_no_forest_not_offered_forest_offered() {
    // (i) Island + printed-cost mana: cast on mana, no offer.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Blue);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(!trace.saw_optional_cost, "no Forest → no alternative cost");
    assert_eq!(runner.life(P1), p1_before);
    assert_eq!(power(&runner, bear), Some(6));

    // Positive: Forest + the same pool → offered.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    assert!(drive_cast(&mut runner, inv, bear, false, None).saw_optional_cost);

    // (ii) Public castability with no mana: Island alone can't, Forest alone can.
    let Setup {
        mut scenario, inv, ..
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Blue);
    let runner = scenario.build();
    assert!(!can_cast_object_now(runner.state(), P0, inv));

    let Setup {
        mut scenario, inv, ..
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    let runner = scenario.build();
    assert!(
        can_cast_object_now(runner.state(), P0, inv),
        "Forest alone makes Invigorate castable through the alternative cost"
    );
}

/// V7 — Phase 1 with two choosable opponents: not offered; the printed cost is
/// still castable. Eliminating or phasing out the second opponent restores the
/// single-recipient offer.
#[test]
fn v7_two_choosable_opponents_not_offered_until_one_remains() {
    // DEFERRED(phase 2): U3 turns this into a recipient prompt; this assertion flips.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(!trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before);
    assert_eq!(runner.life(P2), p2_before);
    assert_eq!(power(&runner, bear), Some(6), "printed cost still castable");

    let Setup {
        mut scenario, inv, ..
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let runner = scenario.build();
    // DEFERRED(phase 2): flips to true once the recipient prompt exists.
    assert!(!can_cast_object_now(runner.state(), P0, inv));

    // (b) Eliminated third seat → exactly one recipient → offered.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::elimination::eliminate_player(runner.state_mut(), P2, &mut events);
    assert!(
        runner.state().players[2].is_eliminated,
        "setup: P2 eliminated"
    );
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before + 3);
    assert_eq!(runner.life(P2), p2_before);

    // (c) Phased-out third seat → exactly one recipient → offered.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::phasing::phase_out_player(runner.state_mut(), P2, &mut events);
    assert!(
        runner.state().players[2].is_phased_out(),
        "setup: P2 phased out"
    );
    let p1_before = runner.life(P1);
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before + 3);
}

/// V8a — CR 119.7 + CR 614.17b doctrine: an opponent who can't gain life is not
/// a recipient, so the cost isn't offered. Q1 — needs manual verification.
#[test]
fn v8a_opponent_who_cant_gain_life_is_not_a_recipient() {
    // Positive control: same builder without the lock.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    assert!(drive_cast(&mut runner, inv, bear, false, None).saw_optional_cost);

    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P1, "Everlasting Torment", EVERLASTING_TORMENT);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    assert!(!drive_cast(&mut runner, inv, bear, true, None).saw_optional_cost);

    let Setup {
        mut scenario, inv, ..
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P1, "Everlasting Torment", EVERLASTING_TORMENT);
    let runner = scenario.build();
    assert!(!can_cast_object_now(runner.state(), P0, inv));
}

/// V8b — a gain replaced down to zero can still happen (CR 614): offered, paid,
/// opponent gains 0.
#[test]
fn v8b_replaced_to_zero_is_still_payable() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P1, "Lifegain Negator", LIFEGAIN_NEGATOR);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(
        runner.life(P1),
        p1_before,
        "replacement reduced the gain to 0"
    );
    assert!(!tapped(&runner, forest));
    assert_eq!(power(&runner, bear), Some(6));
}

/// V8c — the can't-gain lock is evaluated per recipient, relative to its
/// controller: Archfiend under P0 locks P1 (not offered); under P1 it locks only
/// P0 (offered, P1 gains).
#[test]
fn v8c_controller_relative_lock() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_creature_from_oracle(P0, "Archfiend of Despair", 6, 6, ARCHFIEND_OF_DESPAIR);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    assert!(!drive_cast(&mut runner, inv, bear, true, None).saw_optional_cost);

    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_creature_from_oracle(P1, "Archfiend of Despair", 6, 6, ARCHFIEND_OF_DESPAIR);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before + 3);
}

/// V8d — Q1-dependent forced binding in three-player: P2's Archfiend locks P0
/// and P1, so P2 is the only recipient.
#[test]
fn v8d_three_player_lock_leaves_single_recipient() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_creature_from_oracle(P2, "Archfiend of Despair", 6, 6, ARCHFIEND_OF_DESPAIR);
    let mut runner = scenario.build();
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));
    let trace = drive_cast(&mut runner, inv, bear, true, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P2), p2_before + 3);
    assert_eq!(runner.life(P1), p1_before);
}

struct ReplacementFixture {
    runner: GameRunner,
    inv: ObjectId,
    bear: ObjectId,
    strange: ObjectId,
    pest: Option<ObjectId>,
    forest: ObjectId,
}

fn replacement_fixture(with_pest: bool) -> ReplacementFixture {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let strange = scenario
        .add_creature_from_oracle(P1, "Doctor Strange, Surgeon", 2, 2, DOCTOR_STRANGE)
        .id();
    let pest = with_pest.then(|| {
        scenario
            .add_creature_from_oracle(P1, "Pest Rescuer", 1, 1, PEST_RESCUER)
            .id()
    });
    ReplacementFixture {
        runner: scenario.build(),
        inv,
        bear,
        strange,
        pest,
        forest,
    }
}

/// V9a — CR 616.1: the RECIPIENT orders competing life-gain replacements, and
/// the cast resumes. Doubler first: 3 → 6 → 7.
#[test]
fn v9a_recipient_orders_replacements_doubler_first() {
    let mut f = replacement_fixture(true);
    let (p0_before, p1_before) = (f.runner.life(P0), f.runner.life(P1));
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, Some(f.strange));
    assert!(trace.saw_optional_cost);
    assert_eq!(trace.replacement_prompt_player, Some(P1));
    assert_eq!(f.runner.life(P1), p1_before + 7);
    assert_eq!(f.runner.life(P0), p0_before);
    assert!(!tapped(&f.runner, f.forest));
    assert_eq!(power(&f.runner, f.bear), Some(6));
}

/// V9b — the order is honoured: plus-one first gives (3 + 1) × 2 = 8.
#[test]
fn v9b_recipient_orders_replacements_plus_one_first() {
    let mut f = replacement_fixture(true);
    let p1_before = f.runner.life(P1);
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, f.pest);
    assert_eq!(trace.replacement_prompt_player, Some(P1));
    assert_eq!(f.runner.life(P1), p1_before + 8);
    assert_eq!(power(&f.runner, f.bear), Some(6));
}

/// V9s — a single replacement applies without a prompt.
#[test]
fn v9s_single_replacement_applies_without_prompt() {
    let mut f = replacement_fixture(false);
    let p1_before = f.runner.life(P1);
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(trace.replacement_prompt_player, None);
    assert_eq!(f.runner.life(P1), p1_before + 6);
    assert_eq!(power(&f.runner, f.bear), Some(6));
}

/// V9c — resume-carrier invariant: at the recipient's replacement prompt the
/// parked root is still affiliated with the payer's cast, every ordering is a
/// witnessed completion, and answering resumes P0's cast.
#[test]
fn v9c_recipient_prompt_stays_affiliated_with_payer_cast() {
    let mut f = replacement_fixture(true);
    let card_id = f.runner.state().objects[&f.inv].card_id;
    f.runner
        .act(GameAction::CastSpell {
            object_id: f.inv,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");
    for _ in 0..16 {
        match f.runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                f.runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(f.bear)],
                    })
                    .expect("target");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                f.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("accept alt cost");
            }
            WaitingFor::ReplacementChoice { .. } => break,
            other => panic!("unexpected prompt before replacement choice: {other:?}"),
        }
    }
    let state = f.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P1),
        "recipient owns the prompt: {:?}",
        state.waiting_for
    );
    assert!(state.pending_deferred_life_cost_resume.is_some());
    assert!(matches!(
        classify_payment_continuation(state),
        PaymentContinuationState::Affiliated(PaymentContinuationRoot::Spell { object_id, payer, .. })
            if object_id == f.inv && payer == P0
    ));
    let actions = legal_actions(state);
    let choose: Vec<usize> = actions
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, GameAction::ChooseReplacement { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(choose.len(), 2, "two orderings offered: {actions:?}");
    let batch = witness_payment_continuations(state, &actions);
    assert_eq!(batch.status, PaymentContinuationBatchStatus::Complete);
    for i in choose {
        assert!(
            batch.successors[i].is_some(),
            "ordering {:?} must be a witnessed completion",
            actions[i]
        );
    }

    let strange_index = match &f.runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_id == f.strange)
            .expect("Doctor Strange candidate"),
        _ => unreachable!(),
    };
    f.runner
        .act(GameAction::ChooseReplacement {
            index: strange_index,
        })
        .expect("recipient answers");
    let state = f.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "cast resumes to the payer's priority: {:?}",
        state.waiting_for
    );
    assert!(state.stack.iter().any(|e| e.id == f.inv));
    assert!(state.pending_deferred_life_cost_resume.is_none());
}
