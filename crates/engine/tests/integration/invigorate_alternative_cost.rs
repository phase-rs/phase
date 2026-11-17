//! Invigorate — "If you control a Forest, rather than pay this spell's mana cost,
//! you may have an opponent gain 3 life." (CR 118.9 swapped clause order; the
//! life gain is an effect performed as a cost, CR 118.3 + CR 119.3, whose
//! recipient the caster chooses, CR 115.10a).
//!
//! Every fixture Oracle text below is verbatim from the local Scryfall-derived
//! dataset, except "Lifegain Negator" and "Self Lifelock", which are only the
//! clauses the engine's own parser tests pin (not a real card's full text).
//!
//! All tests drive the real cast pipeline through public APIs: `GameAction`s via
//! `GameRunner::act`, recording whether the `OptionalCostChoice` offer appeared.

use engine::ai_support::{
    classify_payment_continuation, legal_actions, witness_payment_continuations,
    PaymentContinuationBatchStatus, PaymentContinuationRoot, PaymentContinuationState,
};
use engine::game::casting::can_cast_object_now;
use engine::game::players::team_life_total;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turn_control::authorized_submitter;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastOpponentChoicePurpose, CastPaymentMode, DeferredLifeGainCost, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const INVIGORATE: &str = "If you control a Forest, rather than pay this spell's mana cost, you may have an opponent gain 3 life.\nTarget creature gets +4/+4 until end of turn.";
const EVERLASTING_TORMENT: &str = "Players can't gain life.\nDamage can't be prevented.\nAll damage is dealt as though its source had wither. (A source with wither deals damage to creatures in the form of -1/-1 counters.)";
const DOCTOR_STRANGE: &str = "Lifelink\nIf you would gain life, you gain twice that much life instead.\nAt the beginning of each combat, if you have at least 10 life more than your starting life total, creatures you control get +2/+2 and gain vigilance until end of turn.";
const PEST_RESCUER: &str = "At the beginning of each upkeep, if you don't control a Pest creature token, create a 1/1 black and green Pest creature token with \"When this token dies, you gain 1 life.\"\nIf you would gain life, you gain that much life plus 1 instead.";
const ARCHFIEND_OF_DESPAIR: &str = "Flying\nYour opponents can't gain life.\nAt the beginning of each end step, each opponent loses life equal to the life that player lost this turn. (Damage causes loss of life.)";
/// The "You can't gain life." CLAUSE only, as pinned by the engine's parser test
/// `static_you_cant_gain_life` (see module doc); no real card's full text.
const SELF_LIFELOCK: &str = "You can't gain life.";
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
    /// `(player, candidates)` of the cast-time opponent prompt, if one was raised.
    recipient_prompt: Option<(PlayerId, Vec<PlayerId>)>,
    recipient_purpose_is_effect_cost: bool,
}

/// Where the shared cast loop stops.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StopAt {
    /// Pass priority until the stack is empty (the spell resolved).
    EmptyStack,
    /// Return at the first `Priority` with the spell's entry on the stack.
    SpellOnStack,
}

/// Cast Invigorate targeting `target`, answering the alternative-cost offer with
/// `pay_alt`, a cast-time recipient prompt with `recipient_pick`, and a
/// replacement-ordering prompt with the candidate whose source is `repl_pick`,
/// then pass priority until the stack is empty. Panics on any unexpected prompt
/// (so a `None` pick also asserts that prompt never appeared).
fn drive_cast(
    runner: &mut GameRunner,
    inv: ObjectId,
    target: ObjectId,
    pay_alt: bool,
    repl_pick: Option<ObjectId>,
    recipient_pick: Option<PlayerId>,
) -> CastTrace {
    cast_loop(
        runner,
        inv,
        target,
        pay_alt,
        repl_pick,
        recipient_pick,
        StopAt::EmptyStack,
    )
}

/// Like `drive_cast` with the alternative cost accepted, but returns at the first
/// `Priority` whose stack holds Invigorate's entry, without passing priority.
/// Panics if a `Priority` is reached without that entry, so the caller always
/// holds a live stack entry.
fn drive_cast_until_on_stack(
    runner: &mut GameRunner,
    inv: ObjectId,
    target: ObjectId,
    recipient_pick: Option<PlayerId>,
) -> CastTrace {
    cast_loop(
        runner,
        inv,
        target,
        true,
        None,
        recipient_pick,
        StopAt::SpellOnStack,
    )
}

fn cast_loop(
    runner: &mut GameRunner,
    inv: ObjectId,
    target: ObjectId,
    pay_alt: bool,
    repl_pick: Option<ObjectId>,
    recipient_pick: Option<PlayerId>,
    stop_at: StopAt,
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
                    .act(GameAction::DecideOptionalCost {
                        index: if pay_alt { 0 } else { 1 },
                    })
                    .expect("optional cost decision should succeed");
            }
            WaitingFor::ChooseGiftRecipient {
                player,
                candidates,
                purpose,
                ..
            } => {
                trace.recipient_prompt = Some((player, candidates));
                trace.recipient_purpose_is_effect_cost =
                    matches!(purpose, CastOpponentChoicePurpose::EffectCost { .. });
                let opponent = recipient_pick.expect("unexpected recipient prompt");
                runner
                    .act(GameAction::ChooseGiftRecipient { opponent })
                    .expect("recipient choice should succeed");
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
            WaitingFor::Priority { .. } if stop_at == StopAt::SpellOnStack => {
                assert!(
                    runner.state().stack.iter().any(|e| e.id == inv),
                    "Invigorate never reached the stack: {:?}",
                    runner.state().stack
                );
                return trace;
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

    let trace = drive_cast(&mut runner, inv, bear, true, None, None);

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

    let trace = drive_cast(&mut runner, inv, bear, false, None, None);

    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before, "no life gain when declined");
    assert!(
        lands.iter().all(|&l| tapped(&runner, l)),
        "printed {{2}}{{G}} paid"
    );
    assert_eq!(power(&runner, bear), Some(6));
}

/// V6 — CR 118.9 + CR 601.2b: without a Forest the alternative cost is not
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
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
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
    assert!(drive_cast(&mut runner, inv, bear, false, None, None).saw_optional_cost);

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

/// V7′a — CR 601.2h + CR 115.10a: with two choosable opponents the alternative
/// cost IS offered, and the payer is asked which opponent gains the life. No mana
/// is available, so the cast can only complete through the alternative cost.
#[test]
fn v7a_two_opponents_prompt_payer_and_pay_chosen_first() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));

    let trace = drive_cast(&mut runner, inv, bear, true, None, Some(P1));

    assert!(trace.saw_optional_cost, "alternative cost must be offered");
    assert_eq!(trace.recipient_prompt, Some((P0, vec![P1, P2])));
    assert!(trace.recipient_purpose_is_effect_cost);
    assert_eq!(runner.life(P1), p1_before + 3, "chosen opponent gains 3");
    assert_eq!(runner.life(P2), p2_before, "other opponent unchanged");
    assert!(!tapped(&runner, forest), "no mana spent");
    assert_eq!(power(&runner, bear), Some(6), "pump resolved");
}

/// V7′b — the answer is honoured: choosing the second candidate pays it, not the
/// first.
#[test]
fn v7b_two_opponents_choosing_the_other_opponent_is_honoured() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));

    let trace = drive_cast(&mut runner, inv, bear, true, None, Some(P2));

    assert_eq!(trace.recipient_prompt, Some((P0, vec![P1, P2])));
    assert_eq!(runner.life(P2), p2_before + 3);
    assert_eq!(runner.life(P1), p1_before);
    assert_eq!(power(&runner, bear), Some(6));
}

/// V7′-cast — public castability with two choosable opponents and only a Forest.
#[test]
fn v7_cast_two_opponents_forest_alone_is_castable() {
    let Setup {
        mut scenario, inv, ..
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let runner = scenario.build();
    assert!(can_cast_object_now(runner.state(), P0, inv));
}

/// V7′c / V7′d — an eliminated or phased-out third seat leaves exactly one
/// recipient, which binds automatically with no prompt.
#[test]
fn v7cd_single_remaining_recipient_auto_binds_without_prompt() {
    // (c) Eliminated third seat.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::elimination::eliminate_player(
        runner.state_mut(),
        P2,
        engine::types::events::LossReason::Conceded,
        &mut events,
    );
    assert!(
        runner.state().players[2].is_eliminated,
        "setup: P2 eliminated"
    );
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(trace.recipient_prompt, None);
    assert_eq!(runner.life(P1), p1_before + 3);
    assert_eq!(runner.life(P2), p2_before);

    // (d) Phased-out third seat.
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
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(trace.recipient_prompt, None);
    assert_eq!(runner.life(P1), p1_before + 3);
}

/// V8a — CR 119.7 + CR 614.17b (Invigorate ruling 2020-08-07): an opponent who
/// can't gain life is not a recipient, so with no mana window the cost isn't
/// offered.
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
    assert!(drive_cast(&mut runner, inv, bear, false, None, None).saw_optional_cost);

    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P1, "Everlasting Torment", EVERLASTING_TORMENT);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    assert!(!drive_cast(&mut runner, inv, bear, true, None, None).saw_optional_cost);

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
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
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
    assert!(!drive_cast(&mut runner, inv, bear, true, None, None).saw_optional_cost);

    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_creature_from_oracle(P1, "Archfiend of Despair", 6, 6, ARCHFIEND_OF_DESPAIR);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(runner.life(P1), p1_before + 3);
}

/// V8d — CR 119.7 forced binding in three-player: P2's Archfiend locks P0
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
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
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
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, Some(f.strange), None);
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
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, f.pest, None);
    assert_eq!(trace.replacement_prompt_player, Some(P1));
    assert_eq!(f.runner.life(P1), p1_before + 8);
    assert_eq!(power(&f.runner, f.bear), Some(6));
}

/// V9s — a single replacement applies without a prompt.
#[test]
fn v9s_single_replacement_applies_without_prompt() {
    let mut f = replacement_fixture(false);
    let p1_before = f.runner.life(P1);
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, None, None);
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
                    .act(GameAction::DecideOptionalCost { index: 0 })
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

// ---------------------------------------------------------------------------
// Phase 2: the recipient prompt with two or more choosable opponents.
// ---------------------------------------------------------------------------

/// Start casting Invigorate with the alternative cost accepted and stop at the
/// cast-time recipient prompt. Returns the prompt (cloned) for equality checks.
fn to_recipient_prompt(runner: &mut GameRunner, inv: ObjectId, target: ObjectId) -> WaitingFor {
    let card_id = runner.state().objects[&inv].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: inv,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target)],
                    })
                    .expect("target selection should succeed");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { index: 0 })
                    .expect("accept the alternative cost");
            }
            prompt @ WaitingFor::ChooseGiftRecipient { .. } => return prompt,
            other => panic!("unexpected prompt before the recipient prompt: {other:?}"),
        }
    }
    panic!("recipient prompt never raised");
}

/// Pass priority (finalizing any mana payment) until the stack is empty.
fn finish_resolution(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should succeed");
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    panic!("stack never emptied");
}

/// V8e — CR 119.7 (Invigorate ruling 2020-08-07): three players, one
/// opponent can't gain life → the other is the only recipient and binds
/// automatically, with no prompt. Positive control without the lock prompts.
#[test]
fn v8e_three_players_one_self_locked_auto_binds_other() {
    // Positive control: no lock → prompt between both opponents.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let trace = drive_cast(&mut runner, inv, bear, true, None, Some(P1));
    assert_eq!(trace.recipient_prompt, Some((P0, vec![P1, P2])));

    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P2, "Self Lifelock", SELF_LIFELOCK);
    let mut runner = scenario.build();
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(trace.recipient_prompt, None, "single recipient auto-binds");
    assert_eq!(runner.life(P1), p1_before + 3);
    assert_eq!(runner.life(P2), p2_before);
    assert_eq!(power(&runner, bear), Some(6));
}

/// V8f — four players, one opponent locked → the prompt offers only the other
/// two; answering the locked player is refused without changing the prompt.
#[test]
fn v8f_four_players_one_locked_prompts_between_remaining_two() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(4, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P3, "Self Lifelock", SELF_LIFELOCK);
    let mut runner = scenario.build();
    let (p1_before, p2_before, p3_before) = (runner.life(P1), runner.life(P2), runner.life(P3));

    let prompt = to_recipient_prompt(&mut runner, inv, bear);
    let WaitingFor::ChooseGiftRecipient {
        player, candidates, ..
    } = &prompt
    else {
        unreachable!()
    };
    assert_eq!((*player, candidates.clone()), (P0, vec![P1, P2]));

    // Hostile: the locked player is not a candidate.
    assert!(runner
        .act(GameAction::ChooseGiftRecipient { opponent: P3 })
        .is_err());
    assert_eq!(runner.state().waiting_for, prompt, "prompt unchanged");

    runner
        .act(GameAction::ChooseGiftRecipient { opponent: P2 })
        .expect("P2 is a candidate");
    finish_resolution(&mut runner);
    assert_eq!(runner.life(P2), p2_before + 3);
    assert_eq!(runner.life(P1), p1_before);
    assert_eq!(runner.life(P3), p3_before);
    assert_eq!(power(&runner, bear), Some(6));
}

/// CR 104.3a + CR 118.3 + CR 601.2h: a conceding opponent leaves immediately
/// and cannot receive the pending life-gain cost. Rejecting that stale answer
/// preserves the caster's prompt so the remaining opponent can be chosen.
#[test]
fn recipient_who_concedes_after_the_prompt_cannot_pay_the_life_gain_cost() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let p2_before = runner.life(P2);
    let prompt = to_recipient_prompt(&mut runner, inv, bear);
    assert!(matches!(
        prompt,
        WaitingFor::ChooseGiftRecipient { player, ref candidates, purpose: CastOpponentChoicePurpose::EffectCost { .. }, .. }
            if player == P0 && candidates == &vec![P1, P2]
    ));
    let stack_before_choice = runner.state().stack.len();

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("an opponent can concede while the caster owns the prompt");
    let prompt_after_concession = runner.state().waiting_for.clone();
    assert!(matches!(
        prompt_after_concession,
        WaitingFor::ChooseGiftRecipient { player, .. } if player == P0
    ));
    assert!(runner
        .act(GameAction::ChooseGiftRecipient { opponent: P1 })
        .is_err());
    assert_eq!(runner.state().waiting_for, prompt_after_concession);
    assert_eq!(runner.state().stack.len(), stack_before_choice);
    assert_eq!(runner.life(P2), p2_before);

    runner
        .act(GameAction::ChooseGiftRecipient { opponent: P2 })
        .expect("the remaining opponent is still eligible");
    assert_eq!(runner.life(P2), p2_before + 3);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].id, inv);
    finish_resolution(&mut runner);
    assert_eq!(runner.life(P2), p2_before + 3);
    assert_eq!(power(&runner, bear), Some(6));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// V8g — controller-relative lock in four players: an Archfiend under P1 locks
/// P0, P2 and P3, so P1 is the only recipient and binds automatically.
#[test]
fn v8g_four_players_archfiend_leaves_its_controller_only() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(4, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_creature_from_oracle(P1, "Archfiend of Despair", 6, 6, ARCHFIEND_OF_DESPAIR);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(trace.recipient_prompt, None);
    assert_eq!(runner.life(P1), p1_before + 3);
}

/// Two-Headed Giant (4 seats: P0+P1 vs P2+P3), Invigorate in P0's hand.
fn two_headed_giant_setup() -> Setup {
    setup(GameScenario::new_with_format(
        FormatConfig::two_headed_giant(),
        4,
        42,
    ))
}

/// V2HG-a + V2HG-d — CR 102.3 + CR 810: in Two-Headed Giant the candidates are
/// exactly the two opposing players (the caster's teammate is not an opponent),
/// the caster alone may answer, and naming the teammate is refused.
#[test]
fn v2hg_a_candidates_are_the_opposing_team() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = two_headed_giant_setup();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let prompt = to_recipient_prompt(&mut runner, inv, bear);
    let WaitingFor::ChooseGiftRecipient {
        player,
        candidates,
        purpose,
        ..
    } = &prompt
    else {
        unreachable!()
    };
    assert_eq!((*player, candidates.clone()), (P0, vec![P2, P3]));
    assert!(matches!(
        purpose,
        CastOpponentChoicePurpose::EffectCost { .. }
    ));
    // V2HG-d: teams don't change who submits.
    assert_eq!(authorized_submitter(runner.state()), Some(P0));

    // Hostile: the teammate is not a candidate.
    assert!(runner
        .act(GameAction::ChooseGiftRecipient { opponent: P1 })
        .is_err());
    assert_eq!(runner.state().waiting_for, prompt, "prompt unchanged");

    runner
        .act(GameAction::ChooseGiftRecipient { opponent: P2 })
        .expect("P2 is a candidate");
    finish_resolution(&mut runner);
    assert_eq!(power(&runner, bear), Some(6), "reach-guard: cast completed");
}

/// V2HG-b — CR 810.9: the opposing team shares a life total, so choosing either
/// opposing head raises that team's total by exactly 3; the caster's team is
/// unchanged.
#[test]
fn v2hg_b_gain_lands_in_the_opposing_team_total() {
    for pick in [P3, P2] {
        let Setup {
            mut scenario,
            inv,
            bear,
        } = two_headed_giant_setup();
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        let (them_before, us_before) = (
            team_life_total(runner.state(), P2),
            team_life_total(runner.state(), P0),
        );

        let trace = drive_cast(&mut runner, inv, bear, true, None, Some(pick));

        assert_eq!(trace.recipient_prompt, Some((P0, vec![P2, P3])));
        assert_eq!(team_life_total(runner.state(), P2), them_before + 3);
        assert_eq!(
            team_life_total(runner.state(), P3),
            team_life_total(runner.state(), P2)
        );
        assert_eq!(team_life_total(runner.state(), P0), us_before);
        assert!(!tapped(&runner, forest));
        assert_eq!(power(&runner, bear), Some(6));
    }
}

/// V2HG-c — CR 119.7 + CR 810.9g: a can't-gain lock on ONE opposing head locks
/// the whole opposing team, so nobody can be chosen and the alternative cost is
/// not offered (in free-for-all the same one-seat lock auto-binds the other seat,
/// V8e). Positive control without the lock in the same test.
#[test]
fn v2hg_c_lock_on_one_opposing_head_locks_the_team() {
    // Positive control: no lock → offered and prompted; Forest alone castable.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = two_headed_giant_setup();
    scenario.add_basic_land(P0, ManaColor::Green);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    let trace = drive_cast(&mut runner, inv, bear, true, None, Some(P2));
    assert_eq!(trace.recipient_prompt, Some((P0, vec![P2, P3])));

    let Setup {
        mut scenario, inv, ..
    } = two_headed_giant_setup();
    scenario.add_basic_land(P0, ManaColor::Green);
    let runner = scenario.build();
    assert!(can_cast_object_now(runner.state(), P0, inv));

    // (i) Locked: cast on mana, no offer, team totals unchanged.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = two_headed_giant_setup();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P2, "Self Lifelock", SELF_LIFELOCK);
    printed_cost_pool(&mut scenario, inv);
    let mut runner = scenario.build();
    let (them_before, us_before) = (
        team_life_total(runner.state(), P2),
        team_life_total(runner.state(), P0),
    );
    let trace = drive_cast(&mut runner, inv, bear, true, None, None);
    assert!(
        !trace.saw_optional_cost,
        "no choosable recipient → not offered"
    );
    assert_eq!(team_life_total(runner.state(), P2), them_before);
    assert_eq!(team_life_total(runner.state(), P0), us_before);
    assert_eq!(power(&runner, bear), Some(6), "paid with mana");

    // (ii) Locked, Forest only, no mana: not castable.
    let Setup {
        mut scenario, inv, ..
    } = two_headed_giant_setup();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P2, "Self Lifelock", SELF_LIFELOCK);
    let runner = scenario.build();
    assert!(!can_cast_object_now(runner.state(), P0, inv));
}

/// V11 — a non-candidate answer is refused without mutating anything; a
/// candidate answer then succeeds. Also covers an eliminated fourth seat.
#[test]
fn v11_non_candidate_answer_is_refused_without_mutation() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let prompt = to_recipient_prompt(&mut runner, inv, bear);
    let lives = (runner.life(P0), runner.life(P1), runner.life(P2));

    assert!(runner
        .act(GameAction::ChooseGiftRecipient { opponent: P0 })
        .is_err());
    assert_eq!(runner.state().waiting_for, prompt, "prompt unchanged");
    assert_eq!((runner.life(P0), runner.life(P1), runner.life(P2)), lives);

    runner
        .act(GameAction::ChooseGiftRecipient { opponent: P1 })
        .expect("reach-guard: a candidate answer succeeds");
    assert_eq!(runner.life(P1), lives.1 + 3);

    // Four players, P3 eliminated before the cast: P3 is not a candidate.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(4, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::elimination::eliminate_player(
        runner.state_mut(),
        P3,
        engine::types::events::LossReason::Conceded,
        &mut events,
    );
    let prompt = to_recipient_prompt(&mut runner, inv, bear);
    let WaitingFor::ChooseGiftRecipient { candidates, .. } = &prompt else {
        unreachable!()
    };
    assert_eq!(candidates, &vec![P1, P2]);
    assert!(runner
        .act(GameAction::ChooseGiftRecipient { opponent: P3 })
        .is_err());
    assert_eq!(runner.state().waiting_for, prompt, "prompt unchanged");
}

/// Invigorate in a three-player game where `owner` controls Doctor Strange and
/// Pest Rescuer (two competing life-gain replacements).
fn three_player_replacement_fixture(owner: PlayerId) -> ReplacementFixture {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let strange = scenario
        .add_creature_from_oracle(owner, "Doctor Strange, Surgeon", 2, 2, DOCTOR_STRANGE)
        .id();
    let pest = scenario
        .add_creature_from_oracle(owner, "Pest Rescuer", 1, 1, PEST_RESCUER)
        .id();
    ReplacementFixture {
        runner: scenario.build(),
        inv,
        bear,
        strange,
        pest: Some(pest),
        forest,
    }
}

/// V16 — CR 616.1: after the payer picks P2, the RECIPIENT (P2) orders its own
/// competing replacements, and the payer's cast resumes. Sibling: picking P1 on
/// the same board raises no replacement prompt (the replacements are P2's "you").
#[test]
fn v16_chosen_recipient_orders_its_replacements_and_cast_resumes() {
    let mut f = three_player_replacement_fixture(P2);
    let (p1_before, p2_before) = (f.runner.life(P1), f.runner.life(P2));
    let trace = drive_cast(
        &mut f.runner,
        f.inv,
        f.bear,
        true,
        Some(f.strange),
        Some(P2),
    );
    assert_eq!(trace.recipient_prompt, Some((P0, vec![P1, P2])));
    assert_eq!(trace.replacement_prompt_player, Some(P2));
    assert_eq!(f.runner.life(P2), p2_before + 7, "3 doubled to 6, plus 1");
    assert_eq!(f.runner.life(P1), p1_before);
    assert!(!tapped(&f.runner, f.forest));
    assert_eq!(power(&f.runner, f.bear), Some(6));

    // Sibling: P1 chosen on the same board → no replacement prompt.
    let mut f = three_player_replacement_fixture(P2);
    let p1_before = f.runner.life(P1);
    let trace = drive_cast(&mut f.runner, f.inv, f.bear, true, None, Some(P1));
    assert_eq!(trace.replacement_prompt_player, None);
    assert_eq!(f.runner.life(P1), p1_before + 3);

    // Carrier: at P2's prompt the parked root is still the payer's cast.
    let mut f = three_player_replacement_fixture(P2);
    to_recipient_prompt(&mut f.runner, f.inv, f.bear);
    f.runner
        .act(GameAction::ChooseGiftRecipient { opponent: P2 })
        .expect("choose P2");
    let state = f.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P2),
        "recipient owns the prompt: {:?}",
        state.waiting_for
    );
    assert!(state.pending_deferred_life_cost_resume.is_some());
    assert!(matches!(
        classify_payment_continuation(state),
        PaymentContinuationState::Affiliated(PaymentContinuationRoot::Spell { object_id, payer, .. })
            if object_id == f.inv && payer == P0
    ));
    let strange_index = match &state.waiting_for {
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
    assert!(
        matches!(f.runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "cast resumes to the payer's priority: {:?}",
        f.runner.state().waiting_for
    );
}

/// V17 — hostile identity: an effect-cost answer never latches a Gift recipient
/// into the spell's context.
#[test]
fn v17_effect_cost_answer_does_not_latch_gift_recipient() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let p2_before = runner.life(P2);

    let trace = drive_cast_until_on_stack(&mut runner, inv, bear, Some(P2));
    assert_eq!(trace.recipient_prompt, Some((P0, vec![P1, P2])));
    // Reach-guard: the cast finalized (cost paid) before this Priority.
    assert_eq!(runner.life(P2), p2_before + 3);

    let entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == inv)
        .expect("Invigorate on the stack");
    let StackEntryKind::Spell { ability, .. } = &entry.kind else {
        panic!("not a spell entry: {:?}", entry.kind);
    };
    let Some(ability) = ability else {
        panic!("finalized spell entry has no ability");
    };
    assert!(ability.context.gift_recipient.is_none());

    finish_resolution(&mut runner);
    assert_eq!(power(&runner, bear), Some(6));
}

/// V18 — CR 601.2 rollback: cancelling at the recipient prompt backs out the
/// whole cast with nothing paid; casting again raises the prompt again.
#[test]
fn v18_cancel_at_recipient_prompt_backs_out_the_cast() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let (p1_before, p2_before) = (runner.life(P1), runner.life(P2));

    to_recipient_prompt(&mut runner, inv, bear);
    runner
        .act(GameAction::CancelCast)
        .expect("cancel is accepted at the prompt");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&inv].zone, Zone::Hand);
    assert_eq!((runner.life(P1), runner.life(P2)), (p1_before, p2_before));
    assert!(!tapped(&runner, forest));

    // Re-cast: the prompt appears again (nothing stale).
    let prompt = to_recipient_prompt(&mut runner, inv, bear);
    assert!(matches!(
        prompt,
        WaitingFor::ChooseGiftRecipient { ref candidates, .. } if candidates == &vec![P1, P2]
    ));
}

/// V13 + V14 — AI legality and routing at the recipient prompt: every candidate
/// and only candidates is a legal answer, `CancelCast` is legal (as at a Gift
/// recipient prompt), and the caster is the acting authority.
#[test]
fn v13_v14_prompt_legal_actions_and_authority() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new_n_player(3, 42));
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    to_recipient_prompt(&mut runner, inv, bear);

    let actions = legal_actions(runner.state());
    let recipients: std::collections::BTreeSet<PlayerId> = actions
        .iter()
        .filter_map(|a| match a {
            GameAction::ChooseGiftRecipient { opponent } => Some(*opponent),
            _ => None,
        })
        .collect();
    assert_eq!(recipients, [P1, P2].into_iter().collect());
    assert!(actions.contains(&GameAction::CancelCast), "{actions:?}");
    assert_eq!(authorized_submitter(runner.state()), Some(P0));
}

// ---------------------------------------------------------------------------
// The cost is paid at the CR 601.2h payment, after the CR 601.2g mana window.
// ---------------------------------------------------------------------------

const SPHERE_OF_RESISTANCE: &str = "Spells cost {1} more to cast.";
const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";
const EMERALD_MEDALLION: &str = "Green spells you cast cost {1} less to cast.";

/// P0 controls a Forest, Ashnod's Altar and Archfiend of Despair (P1 can't gain
/// life); Sphere of Resistance makes Invigorate's total cost through the
/// alternative cost {1} (CR 118.9d). `with_medallion` adds Emerald Medallion,
/// which reduces that {1} to {0}.
fn archfiend_board(with_medallion: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    let altar = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let archfiend = scenario
        .add_creature_from_oracle(P0, "Archfiend of Despair", 6, 6, ARCHFIEND_OF_DESPAIR)
        .id();
    scenario.add_artifact_from_oracle(P1, "Sphere of Resistance", SPHERE_OF_RESISTANCE);
    if with_medallion {
        scenario.add_artifact_from_oracle(P0, "Emerald Medallion", EMERALD_MEDALLION);
    }
    (scenario.build(), inv, bear, altar, archfiend)
}

/// Start casting Invigorate paying manually and answer targets and the
/// alternative-cost offer (`accept_alt`), stopping at the first prompt that is
/// none of those. Returns whether the alternative cost was offered.
fn manual_cast_to_next_prompt(
    runner: &mut GameRunner,
    inv: ObjectId,
    bear: ObjectId,
    accept_alt: bool,
) -> bool {
    let card_id = runner.state().objects[&inv].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: inv,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast should start");
    let mut offered = false;
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bear)],
                    })
                    .expect("target selection should succeed");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                offered = true;
                runner
                    .act(GameAction::DecideOptionalCost {
                        index: if accept_alt { 0 } else { 1 },
                    })
                    .expect("answer the alternative cost");
            }
            _ => return offered,
        }
    }
    panic!("cast never reached a settled prompt");
}

/// Cast Invigorate paying manually, accept the alternative cost, and stop at the
/// mana-payment window. Panics if the alternative cost is not offered or no
/// window opens.
fn to_manual_mana_window(runner: &mut GameRunner, inv: ObjectId, bear: ObjectId) {
    let offered = manual_cast_to_next_prompt(runner, inv, bear, true);
    assert!(offered, "alternative cost must be offered");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "a mana-payment window must open, got {:?}",
        runner.state().waiting_for
    );
}

/// The `{1}` Sphere of Resistance adds to the zero-mana alternative cost: a
/// concrete one-generic cost, distinct from both `NoCost` and a concrete `{0}`.
fn one_generic() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![],
    }
}

/// V19 — CR 118.9d + CR 601.2g + CR 601.2h + CR 119.7: with Sphere alone the
/// alternative cost's total is {1}, so the cast has a mana payment and therefore
/// a mana-ability window before the life-gain cost is paid. Archfiend's lock on
/// P1 does not hide the alternative cost: sacrificing Archfiend to Ashnod's Altar
/// in that window removes the lock, one of the {C}{C} pays the {1}, and the commit
/// pays the cost so P1 gains 3. The gain is not made before the window.
#[test]
fn v19_mana_ability_window_removes_the_lock_before_the_gain_is_paid() {
    let (mut runner, inv, bear, altar, archfiend) = archfiend_board(false);
    let p1_before = runner.life(P1);

    to_manual_mana_window(&mut runner, inv, bear);
    let pending = runner
        .state()
        .pending_cast
        .as_ref()
        .expect("pending manual cast");
    // CR 601.2f: the locked obligation is exactly Sphere's {1} on the zero-mana
    // base — neither `NoCost` nor a concrete {0}.
    assert_eq!(pending.cost, one_generic());
    assert!(matches!(
        pending.deferred_life_gain_cost,
        Some(DeferredLifeGainCost::Unpaid { .. })
    ));
    assert_eq!(
        runner.life(P1),
        p1_before,
        "CR 601.2g: no cost is paid before the mana window"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ashnod's Altar is activatable during payment");
    runner
        .act(GameAction::SelectCards {
            cards: vec![archfiend],
        })
        .expect("sacrificing Archfiend for mana is legal");
    assert_eq!(runner.state().objects[&archfiend].zone, Zone::Graveyard);
    let pool_before = runner.state().players[0].mana_pool.total();
    assert_eq!(pool_before, 2, "the Altar produced {{C}}{{C}}");
    runner
        .act(GameAction::PassPriority)
        .expect("the payment commits once the lock is gone");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "exactly one of the two mana paid Sphere's {{1}}"
    );
    assert_eq!(runner.life(P1), p1_before + 3, "P1 gains 3 at payment");
    assert_eq!(runner.state().stack.len(), 1);
    assert!(runner.state().stack.iter().any(|e| e.id == inv));
    finish_resolution(&mut runner);
    assert_eq!(power(&runner, bear), Some(6));
    assert_eq!(runner.life(P1), p1_before + 3, "the cost is paid once");
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// V19 negative control — CR 107.4d + CR 118.5 + CR 601.2g + CR 601.2h + CR 118.3:
/// Sphere and Emerald Medallion bring the alternative cost's total to {0}, which
/// needs no mana payment, so no mana-ability window opens and the life-gain cost
/// is paid at the commit. With Archfiend still out P1 can't gain life, so the cost
/// can't be paid and the alternative cost is not available: Ashnod's Altar can't
/// be used to remove the lock, and nothing is paid. Positive control on the same
/// board without Archfiend: the {0} alternative completes without any window.
#[test]
fn v19_zero_obligation_opens_no_mana_window() {
    let (mut runner, inv, bear, _altar, archfiend) = archfiend_board(true);
    let p1_before = runner.life(P1);

    let offered = manual_cast_to_next_prompt(&mut runner, inv, bear, true);
    assert!(
        !offered,
        "CR 118.3: a cost that can't be paid can't be chosen as the alternative"
    );
    assert!(
        !matches!(
            runner
                .state()
                .pending_cast
                .as_ref()
                .map(|p| &p.deferred_life_gain_cost),
            Some(Some(_))
        ),
        "no life-gain cost was accepted"
    );
    assert_eq!(runner.life(P1), p1_before, "nothing is paid");
    assert_eq!(
        runner.state().objects[&archfiend].zone,
        Zone::Battlefield,
        "Archfiend was not sacrificed for mana"
    );

    // Positive control: without Archfiend the same {0} total completes the cast
    // with no mana-payment window at any point.
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_artifact_from_oracle(P1, "Sphere of Resistance", SPHERE_OF_RESISTANCE);
    scenario.add_artifact_from_oracle(P0, "Emerald Medallion", EMERALD_MEDALLION);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);
    let offered = manual_cast_to_next_prompt(&mut runner, inv, bear, true);
    assert!(offered, "the alternative cost is offered");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no mana-payment window opens at a {{0}} total: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.iter().any(|e| e.id == inv));
    assert_eq!(runner.life(P1), p1_before + 3, "paid at the commit");
}

/// V20 / F1 — CR 118.9d + CR 601.2f + CR 601.2g + CR 601.2h: Sphere of Resistance's
/// {1} applies to the life-gain alternative cost, so with only Sphere on the board
/// {1} is owed. Completing the payment with an empty pool is refused and nothing is
/// paid; after the Forest is tapped for exactly one mana the commit spends it and
/// the opponent gains 3 before Invigorate resolves.
#[test]
fn v20_alternative_cast_owes_spheres_tax() {
    let Setup {
        mut scenario,
        inv,
        bear,
    } = setup(GameScenario::new());
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_artifact_from_oracle(P1, "Sphere of Resistance", SPHERE_OF_RESISTANCE);
    let mut runner = scenario.build();
    let p1_before = runner.life(P1);

    to_manual_mana_window(&mut runner, inv, bear);
    assert_eq!(
        runner.state().pending_cast.as_ref().map(|p| &p.cost),
        Some(&one_generic()),
        "{{1}} is owed"
    );
    // With no mana source left and an empty pool the {1} can't be paid: the
    // commit is refused and nothing is paid. (The Forest is tapped as fixture
    // setup so the manual window has no source to draw on.)
    runner
        .state_mut()
        .objects
        .get_mut(&forest)
        .expect("Forest")
        .tapped = true;
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(
        runner.act(GameAction::PassPriority).is_err(),
        "an empty pool can't pay the {{1}}"
    );
    assert_eq!(runner.life(P1), p1_before, "nothing is paid when refused");
    assert!(
        runner.state().pending_cast.is_some(),
        "the cast is still being paid"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
        .state_mut()
        .objects
        .get_mut(&forest)
        .expect("Forest")
        .tapped = false;

    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    let selection = grouped
        .get(&forest)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the engine authors the Forest's mana selection");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap the Forest for mana");
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    assert_eq!(runner.life(P1), p1_before, "not paid before the commit");
    runner.act(GameAction::PassPriority).expect("commit");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the mana was actually spent on Sphere's {{1}}"
    );
    assert_eq!(runner.life(P1), p1_before + 3, "paid at the commit");
    assert!(runner.state().stack.iter().any(|e| e.id == inv));
    assert_eq!(power(&runner, bear), Some(2), "not resolved yet");
    finish_resolution(&mut runner);
    assert_eq!(power(&runner, bear), Some(6));
}

/// CR 601.2h + CR 118.5: a spell whose entire mana cost is {0} has nothing to pay,
/// so even a manual cast completes without a `ManaPayment` window. The window the
/// Invigorate cast opens comes from its deferred life-gain cost, not from a {0}
/// mana component; this guards the shared {0} boundary the Warped Space casts rely on.
#[test]
fn manual_zero_mana_spell_completes_without_a_payment_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let memnite = scenario
        .add_creature_to_hand_from_oracle(P0, "Memnite", 1, 1, "")
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&memnite].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: memnite,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("a zero-mana spell can be cast manually");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "a {{0}} cost opens no payment window, found {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(runner.state().stack.len(), 1);
    finish_resolution(&mut runner);
    assert_eq!(runner.state().objects[&memnite].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// ---------------------------------------------------------------------------
// "Have each other player gain N life" (Reverent Silence, Skyshroud Cutter).
// ---------------------------------------------------------------------------

const REVERENT_SILENCE: &str = "If you control a Forest, rather than pay this spell's mana cost, you may have each other player gain 6 life.\nDestroy all enchantments.";
const SKYSHROUD_CUTTER: &str = "If you control a Forest, rather than pay this spell's mana cost, you may have each other player gain 5 life.";
const GLORIOUS_ANTHEM: &str = "Creatures you control get +1/+1.";

/// Reverent Silence ({3}{G}) in P0's hand, a Forest for P0 and Glorious Anthem
/// under P1.
fn reverent_silence_board(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let anthem = scenario
        .add_enchantment_from_oracle(P1, "Glorious Anthem", GLORIOUS_ANTHEM)
        .id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Reverent Silence", false, REVERENT_SILENCE);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 3,
        shards: vec![ManaCostShard::Green],
    });
    (builder.id(), forest, anthem)
}

/// V21 — CR 118.9 + CR 119.3: Reverent Silence's alternative cost has every other
/// player gain 6 life before the spell resolves; no mana is spent and the
/// enchantments are destroyed.
#[test]
fn v21_reverent_silence_each_other_player_gains_six() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let (silence, forest, anthem) = reverent_silence_board(&mut scenario);
    let mut runner = scenario.build();
    let (p0_before, p1_before, p2_before) = (runner.life(P0), runner.life(P1), runner.life(P2));

    let trace = drive_cast_until_on_stack(&mut runner, silence, silence, None);
    assert!(trace.saw_optional_cost, "alternative cost must be offered");
    assert_eq!(trace.recipient_prompt, None, "nobody is chosen");
    assert_eq!(runner.life(P1), p1_before + 6, "P1 gains 6 at payment");
    assert_eq!(runner.life(P2), p2_before + 6, "P2 gains 6 at payment");
    assert_eq!(runner.life(P0), p0_before, "the caster gains nothing");
    assert!(!tapped(&runner, forest), "no mana spent");

    finish_resolution(&mut runner);
    assert_eq!(runner.state().objects[&anthem].zone, Zone::Graveyard);
}

/// V22 — CR 119.7 + CR 601.2h: one other player who can't gain life makes the
/// "each other player" cost unpayable (no partial payment), so with no mana window
/// a Forest alone can't cast Reverent Silence. Positive control without the lock
/// on the same builder.
#[test]
fn v22_reverent_silence_one_locked_player_makes_the_cost_unpayable() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let (silence, _, _) = reverent_silence_board(&mut scenario);
    let runner = scenario.build();
    assert!(can_cast_object_now(runner.state(), P0, silence));

    let mut scenario = GameScenario::new_n_player(3, 42);
    let (silence, _, _) = reverent_silence_board(&mut scenario);
    scenario.add_enchantment_from_oracle(P2, "Self Lifelock", SELF_LIFELOCK);
    let runner = scenario.build();
    assert!(!can_cast_object_now(runner.state(), P0, silence));
}

/// V23 — CR 102.3 + CR 810.9: "each other player" includes the caster's teammate
/// in Two-Headed Giant. With shared team life, the opposing team gains 12 (two
/// heads) and the caster's team gains 6 (the teammate).
#[test]
fn v23_reverent_silence_reaches_the_teammate_in_two_headed_giant() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    let (silence, _, _) = reverent_silence_board(&mut scenario);
    let mut runner = scenario.build();
    let (us_before, them_before) = (
        team_life_total(runner.state(), P0),
        team_life_total(runner.state(), P2),
    );
    let trace = drive_cast(&mut runner, silence, silence, true, None, None);
    assert!(trace.saw_optional_cost);
    assert_eq!(team_life_total(runner.state(), P0), us_before + 6);
    assert_eq!(team_life_total(runner.state(), P2), them_before + 12);
}

/// V24 — CR 118.9 + CR 119.3: Skyshroud Cutter's alternative cost has the
/// opponent gain 5 life, and the creature enters without mana spent.
#[test]
fn v24_skyshroud_cutter_each_other_player_gains_five() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Skyshroud Cutter", 2, 2, SKYSHROUD_CUTTER);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 3,
        shards: vec![ManaCostShard::Green],
    });
    let cutter = builder.id();
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (runner.life(P0), runner.life(P1));

    let trace = drive_cast(&mut runner, cutter, cutter, true, None, None);
    assert!(trace.saw_optional_cost, "alternative cost must be offered");
    assert_eq!(runner.life(P1), p1_before + 5);
    assert_eq!(runner.life(P0), p0_before);
    assert!(!tapped(&runner, forest), "no mana spent");
    assert_eq!(runner.state().objects[&cutter].zone, Zone::Battlefield);
}

const LEYLINE_OF_ANTICIPATION: &str =
    "If this card is in your opening hand, you may begin the game with it on the battlefield.
You may cast spells as though they had flash.";
const TAINTED_REMEDY: &str =
    "If an opponent would gain life, that player loses that much life instead.";
const PHIAL_OF_GALADRIEL: &str = "If you would draw a card while you have no cards in hand, draw two cards instead.\nIf you would gain life while you have 5 or less life, you gain twice that much life instead.\n{T}: Add one mana of any color.";

/// F3 — CR 101.4 + CR 119.3 + CR 810.9a: "each other player gain 6 life" gains are
/// simultaneous. In Two-Headed Giant each Phial of Galadriel reads the opposing
/// team's life total (5 = 3 + 2) before either gain happens, so both gains double:
/// 5 + 12 + 12 = 29, not the 23 a gain-by-gain settlement yields. Each recipient is
/// paid once, and the caster's teammate gains the plain 6.
#[test]
fn v25_each_other_player_gains_settle_together_under_team_life_replacements() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    let (silence, forest, _anthem) = reverent_silence_board(&mut scenario);
    scenario.add_artifact_from_oracle(P2, "Phial of Galadriel", PHIAL_OF_GALADRIEL);
    scenario.add_artifact_from_oracle(P3, "Phial of Galadriel", PHIAL_OF_GALADRIEL);
    scenario.with_life(P2, 3);
    scenario.with_life(P3, 2);
    let mut runner = scenario.build();
    assert_eq!(team_life_total(runner.state(), P2), 5);
    let (p0_before, p1_before) = (runner.life(P0), runner.life(P1));

    let trace = drive_cast(&mut runner, silence, silence, true, None, None);
    assert!(trace.saw_optional_cost, "alternative cost must be offered");
    assert_eq!(trace.replacement_prompt_player, None, "no ordering choice");
    assert_eq!(runner.life(P2), 3 + 12, "P2's gain doubled once");
    assert_eq!(runner.life(P3), 2 + 12, "P3's gain doubled once");
    assert_eq!(team_life_total(runner.state(), P2), 29);
    assert_eq!(runner.life(P1), p1_before + 6, "the teammate gains 6");
    assert_eq!(runner.life(P0), p0_before, "the caster gains nothing");
    assert!(!tapped(&runner, forest), "no mana spent");
}

/// F3 — CR 101.4 + CR 616.1: competing life-gain replacements on several
/// recipients surface their ordering choices in APNAP order, not seat order. P2
/// is the active player and P0 casts the printed sorcery Reverent Silence during
/// P2's turn under Leyline of Anticipation (CR 307.1 + CR 601.3b), so the
/// recipients P1, P2, P3 are asked as P2, P3, P1. Doctor Strange doubles and Pest
/// Rescuer adds one, so every recipient has an ordering to make; each gain is
/// applied once, with the order each recipient chose (doubler first: 6 doubled to 12, plus 1 = 13).
#[test]
fn v26_recipient_replacement_choices_follow_apnap_order() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P0, "Leyline of Anticipation", LEYLINE_OF_ANTICIPATION);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Reverent Silence", false, REVERENT_SILENCE);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 3,
        shards: vec![ManaCostShard::Green],
    });
    let silence = builder.id();
    let mut strange = Vec::new();
    for owner in [P1, P2, P3] {
        strange.push(
            scenario
                .add_creature_from_oracle(owner, "Doctor Strange, Surgeon", 2, 2, DOCTOR_STRANGE)
                .id(),
        );
        scenario.add_creature_from_oracle(owner, "Pest Rescuer", 1, 1, PEST_RESCUER);
    }
    let mut runner = scenario.build();
    runner.state_mut().active_player = P2;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let before: Vec<i32> = [P1, P2, P3].iter().map(|&p| runner.life(p)).collect();

    let card_id = runner.state().objects[&silence].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: silence,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");
    let mut prompt_order = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { index: 0 })
                    .expect("accept the alternative cost");
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                prompt_order.push(player);
                let index = candidates
                    .iter()
                    .position(|c| strange.contains(&c.source_id))
                    .expect("Doctor Strange is a candidate");
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("replacement choice should succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(
        prompt_order,
        vec![P2, P3, P1],
        "CR 101.4 + CR 616.1: choices are made in APNAP order, not seat order"
    );
    assert!(runner.state().stack.iter().any(|e| e.id == silence));
    for (index, player) in [P1, P2, P3].into_iter().enumerate() {
        assert_eq!(
            runner.life(player),
            before[index] + 13,
            "{player:?}: 6 doubled to 12, plus 1"
        );
    }
}

/// F1 — CR 101.4 + CR 614.6 + CR 810.9a: a gain that Tainted Remedy turns into a
/// life loss is part of the same simultaneous instruction, so it must not change
/// the life total a later recipient's replacement reads. The opposing team is at
/// 10 (5 + 5), so Phial of Galadriel on P3 ("5 or less life") never applies; had
/// P2's substituted loss been applied while staging, the team would read 4 and
/// Phial would double P3's replaced gain into a 12-point loss. Each opposing
/// recipient loses exactly 6 (10 - 12 = -2) with no ordering prompt, and the
/// caster gains nothing and pays no mana.
#[test]
fn v27_substituted_gain_does_not_change_the_life_later_recipients_read() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    let (silence, forest, _anthem) = reverent_silence_board(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Tainted Remedy", TAINTED_REMEDY);
    scenario.add_artifact_from_oracle(P3, "Phial of Galadriel", PHIAL_OF_GALADRIEL);
    scenario.with_life(P2, 5);
    scenario.with_life(P3, 5);
    let mut runner = scenario.build();
    assert_eq!(team_life_total(runner.state(), P2), 10);
    let p0_before = runner.life(P0);

    let card_id = runner.state().objects[&silence].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: silence,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");
    let mut saw_optional_cost = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                saw_optional_cost = true;
                runner
                    .act(GameAction::DecideOptionalCost { index: 0 })
                    .expect("accept the alternative cost");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize");
            }
            // CR 810.8c: the opposing team ends at -2, so it loses at payment.
            WaitingFor::GameOver { winner } => {
                assert_eq!(winner, Some(P0), "the caster's team wins");
                break;
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("no Phial ordering choice may be offered, got {other:?}"),
        }
    }
    assert!(saw_optional_cost, "alternative cost must be offered");
    assert_eq!(runner.life(P2), 5 - 6, "P2's gain became a 6-point loss");
    assert_eq!(
        runner.life(P3),
        5 - 6,
        "P3's gain became a 6-point loss, not 12"
    );
    // CR 810.9: the two losses land on the shared total, 10 - 6 - 6. (The
    // eliminated team no longer reports a team total, so sum the heads.)
    assert_eq!(runner.life(P2) + runner.life(P3), -2);
    assert_eq!(runner.life(P0), p0_before, "the caster gains nothing");
    assert!(!tapped(&runner, forest), "no mana spent");
}

/// F1 — CR 101.4 + CR 614.6 + CR 616.1 + CR 810.9a: a recipient whose ordering
/// answer turns the gain into a substitute is still part of the simultaneous
/// instruction. P2 has Tainted Remedy (P0's) and Doctor Strange competing for
/// their gain and applies Tainted Remedy first, so the gain becomes a 6-point
/// loss. That loss must wait for the rest of the batch: P3's Phial of Galadriel
/// ("5 or less life") reads the opposing team's total before any of the gains,
/// 10 (5 + 5), so it never applies and P3 is offered no ordering choice. Had the
/// answered substitute run before P3 was staged, the team would read 4 and P3
/// would have to order Tainted Remedy against Phial. Each opposing recipient
/// loses exactly 6.
#[test]
fn v28_answered_substitute_does_not_change_the_life_later_recipients_read() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    let (silence, forest, _anthem) = reverent_silence_board(&mut scenario);
    let remedy = scenario
        .add_enchantment_from_oracle(P0, "Tainted Remedy", TAINTED_REMEDY)
        .id();
    scenario.add_creature_from_oracle(P2, "Doctor Strange, Surgeon", 2, 2, DOCTOR_STRANGE);
    scenario.add_artifact_from_oracle(P3, "Phial of Galadriel", PHIAL_OF_GALADRIEL);
    scenario.with_life(P2, 5);
    scenario.with_life(P3, 5);
    let mut runner = scenario.build();
    assert_eq!(team_life_total(runner.state(), P2), 10);
    let p0_before = runner.life(P0);

    let card_id = runner.state().objects[&silence].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: silence,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");
    let mut saw_optional_cost = false;
    let mut prompt_order = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                saw_optional_cost = true;
                runner
                    .act(GameAction::DecideOptionalCost { index: 0 })
                    .expect("accept the alternative cost");
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                prompt_order.push(player);
                assert_eq!(player, P2, "only P2 has competing replacements");
                let index = candidates
                    .iter()
                    .position(|c| c.source_id == remedy)
                    .expect("Tainted Remedy is a candidate");
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("replacement choice should succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize");
            }
            // CR 810.8c: the opposing team ends at -2, so it loses at payment.
            WaitingFor::GameOver { winner } => {
                assert_eq!(winner, Some(P0), "the caster's team wins");
                break;
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(saw_optional_cost, "alternative cost must be offered");
    assert_eq!(
        prompt_order,
        vec![P2],
        "P2 orders Tainted Remedy against Doctor Strange; P3 gets no Phial choice"
    );
    assert_eq!(runner.life(P2), 5 - 6, "P2's gain became a 6-point loss");
    assert_eq!(
        runner.life(P3),
        5 - 6,
        "P3's gain became a 6-point loss, not 12"
    );
    // CR 810.9: the two losses land on the shared total, 10 - 6 - 6.
    assert_eq!(runner.life(P2) + runner.life(P3), -2);
    assert_eq!(runner.life(P0), p0_before, "the caster gains nothing");
    assert!(!tapped(&runner, forest), "no mana spent");
}
