//! Modal-X activation payment lock-in (characterization, NOT a defect regression).
//!
//! Top finding (reviewed plan): there is no modal-X double charge. Seven
//! bracketed probes plus structural guarantees (XMana re-finalize guard,
//! settle lock-once, idempotent concretize_x) prove the modal-X payment route
//! deducts exactly X. These rows are green at base by design; they flip only
//! if payment regresses.
//!
//! Quoted row language from the plan's Verification Matrix:
//! - "Lock-in: Marath damage X=2 (new file) ... Pool 10/10/10/8; counters
//!   6→4; stack 1; `Priority`; resolution P1 −2; `ChooseXValue`+
//!   `TargetSelection` asserted unconditionally ... green at base by design
//!   (characterization); flips only if payment regresses"
//! - "Lock-in: Atalya life X=2 (same file) ... Pool 10/10/8/8; P0 +2 on
//!   resolution ... Same as above"
//! - "Negative guard: `min_x`/over-max `ChooseX` rejected (paired with the
//!   exact-payment positive in the same test)."
//!   Deviation, probed: this route surfaces `min=0` (the printed "X can't be
//!   0" is not stamped into the modal funnel's prompt), so only the over-max
//!   half is locked; see `marath_modal_x_over_max_rejected_at_announcement`.
//!
//! CR 107.3a (X bound to the effect) + CR 601.2b + CR 602.2b (X announced
//! before the cost is paid) + CR 602.2b (activation announcement order:
//! modes, then X, then targets) + CR 601.2h + CR 602.2b (the activation's
//! total cost paid once) + CR 601.2c (target announcement; vacuous for the
//! untargeted life mode).
//!
//! Oracle texts (verbatim):
//! - Marath, Will of the Wild: "Marath enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\n{X}, Remove X +1/+1 counters from Marath: Choose one \u{2014}\n\u{2022} Put X +1/+1 counters on target creature. X can't be 0.\n\u{2022} Marath deals X damage to any target. X can't be 0.\n\u{2022} Create an X/X green Elemental creature token. X can't be 0."
//! - Atalya, Samite Master: "{X}, {T}: Choose one \u{2014}\n\u{2022} Prevent the next X damage that would be dealt to target creature this turn. Spend only white mana on X.\n\u{2022} You gain X life. Spend only white mana on X."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::TargetRef;

const MARATH_ORACLE: &str = "Marath enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\n{X}, Remove X +1/+1 counters from Marath: Choose one \u{2014}\n\u{2022} Put X +1/+1 counters on target creature. X can't be 0.\n\u{2022} Marath deals X damage to any target. X can't be 0.\n\u{2022} Create an X/X green Elemental creature token. X can't be 0.";
const ATALYA_ORACLE: &str = "{X}, {T}: Choose one \u{2014}\n\u{2022} Prevent the next X damage that would be dealt to target creature this turn. Spend only white mana on X.\n\u{2022} You gain X life. Spend only white mana on X.";

fn mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn pool_total(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map_or(0, |p| p.mana_pool.total())
}

/// Drains any `ManaPayment` auto-pay prompts until `Priority`; panics on any
/// other prompt (in particular, an untargeted mode must never ask for
/// targets after X is locked — the empty-slot tail).
fn drain_payment_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => break,
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("auto mana payment must continue");
            }
            other => panic!("unexpected prompt while paying: {other:?}"),
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "payment must complete at Priority, got {}",
        runner.waiting_for_kind()
    );
}

/// Row 1 — Marath modal damage X=2 deducts exactly X: pool 10/10/10/8,
/// counters 6→4, stack 1, `Priority`, resolution deals 2 to P1.
#[test]
fn marath_modal_damage_x2_deducts_exactly_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 10));
    let marath = scenario
        .add_creature_from_oracle(P0, "Marath, Will of the Wild", 0, 0, MARATH_ORACLE)
        .id();
    // The 0/0 body needs counters to survive state-based actions; the copy
    // in the sibling F1 choreography carries 6 the same way.
    scenario.with_counter(marath, CounterType::Plus1Plus1, 6);
    let mut runner = scenario.build();

    assert_eq!(pool_total(&runner, P0), 10, "pool before activation");
    assert_eq!(
        runner.state().objects[&marath]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&6),
        "reach-guard: Marath carries 6 counters before activation"
    );

    // Announce the modal activation; the mode prompt is unconditional.
    let index = runner.state().objects[&marath]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Marath must carry its modal ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: marath,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Marath must succeed");
    let WaitingFor::AbilityModeChoice {
        player,
        is_activated,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the modal activation must prompt for its mode, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 602.2b: the activator answers the mode prompt"
    );
    assert!(
        is_activated,
        "the prompt must be the activated-ability route"
    );
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choosing the damage mode must succeed");
    assert_eq!(pool_total(&runner, P0), 10, "pool after modes");

    // X announcement; the X prompt is unconditional.
    let WaitingFor::ChooseXValue { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the X-cost activation must prompt for X, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2b + CR 602.2b: the activator announces X"
    );
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announcing X=2 must succeed");

    // Targeted mode: targets are announced after X (CR 602.2b); the lock
    // deducts nothing, so the pool still reads 10 here.
    let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the damage mode must prompt for its target, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2c: the ability controller chooses the target"
    );
    assert_eq!(pool_total(&runner, P0), 10, "pool after ChooseX");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("targeting P1 with the X damage must succeed");
    drain_payment_to_priority(&mut runner);

    // Exactly X paid, once: pool 8, two counters removed, ability on stack.
    assert_eq!(pool_total(&runner, P0), 8, "pool after payment");
    assert_eq!(
        runner.state().objects[&marath]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4),
        "the counter leg removes exactly X=2 (CR 107.3i reach-guard)"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the modal ability is on the stack"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 20, "P0 untouched");
    assert_eq!(runner.life(P1), 18, "X=2 deals exactly 2 (CR 107.3a)");
}

/// Row 2 — Atalya modal life X=2 deducts exactly X through the untargeted
/// empty-slot tail: pool 10/10/8/8, P0 +2 on resolution.
#[test]
fn atalya_modal_life_x2_deducts_exactly_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // White pool: the card's "Spend only white mana on X" leg is white.
    scenario.with_mana_pool(P0, mana(ManaType::White, 10));
    let atalya = scenario
        .add_creature_from_oracle(P0, "Atalya, Samite Master", 2, 3, ATALYA_ORACLE)
        .id();
    let mut runner = scenario.build();

    assert_eq!(pool_total(&runner, P0), 10, "pool before activation");
    let index = runner.state().objects[&atalya]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Atalya must carry its modal ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: atalya,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Atalya must succeed");
    let WaitingFor::AbilityModeChoice {
        player,
        is_activated,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the modal activation must prompt for its mode, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 602.2b: the activator answers the mode prompt"
    );
    assert!(
        is_activated,
        "the prompt must be the activated-ability route"
    );
    assert_eq!(pool_total(&runner, P0), 10, "pool at mode prompt");
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choosing the life mode must succeed");
    assert_eq!(pool_total(&runner, P0), 10, "pool after modes");

    // Untargeted mode: X locks and pays at the X-lock return (no targets to
    // wait for), so the pool drops to 8 synchronously with ChooseX.
    let WaitingFor::ChooseXValue { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the X-cost activation must prompt for X, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2b + CR 602.2b: the activator announces X"
    );
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announcing X=2 must succeed");
    assert_eq!(pool_total(&runner, P0), 8, "pool after ChooseX");
    // The drain helper panics on TargetSelection: the untargeted life mode
    // must never ask for targets (empty-slot tail, CR 601.2c vacuous).
    drain_payment_to_priority(&mut runner);
    assert_eq!(pool_total(&runner, P0), 8, "pool after payment");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the modal ability is on the stack"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 22, "X=2 gains exactly 2 life");
    assert_eq!(runner.life(P1), 20, "P1 untouched");
}

/// Negative guard, paired with the positives: an over-max X is rejected at the
/// `ChooseXValue` prompt (the prompt stays open), and the same prompt still
/// accepts the boundary X=max, which then pays exactly (pool 10→4, counters
/// 6→0) and resolves for max damage.
///
/// Deviation from the plan's "min_x ... rejected" half, with probe evidence:
/// this route surfaces `min=0` — the printed "X can't be 0" is not stamped
/// into the modal funnel's prompt minimum — so X=0 announces, pays 0, and
/// resolves to a 0-damage no-op (pool unchanged, P1 −0; probed, not asserted
/// here). The sibling F1 choreography already documents "X=0 deals 0 under
/// both regimes and cannot discriminate". Only the over-max rejection exists
/// to lock, so only it is locked; the min half is reported, not manufactured.
#[test]
fn marath_modal_x_over_max_rejected_at_announcement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 10));
    let marath = scenario
        .add_creature_from_oracle(P0, "Marath, Will of the Wild", 0, 0, MARATH_ORACLE)
        .id();
    scenario.with_counter(marath, CounterType::Plus1Plus1, 6);
    let mut runner = scenario.build();

    let index = runner.state().objects[&marath]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Marath must carry its modal ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: marath,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Marath must succeed");
    let WaitingFor::AbilityModeChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the modal activation must prompt for its mode, got {}",
            runner.waiting_for_kind()
        );
    };
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choosing the damage mode must succeed");

    // The cap follows the scarcest leg: 6 counters < 10 pool, so max=6.
    let WaitingFor::ChooseXValue { player, max, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the X-cost activation must prompt for X, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2b + CR 602.2b: the activator announces X"
    );
    assert_eq!(
        max, 6,
        "the prompt maximum tracks the counter leg (6 counters < 10 pool)"
    );
    assert!(
        runner.act(GameAction::ChooseX { value: max + 1 }).is_err(),
        "announcing X={} above the max of {max} must be rejected",
        max + 1
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "the rejection must leave the X prompt open, got {}",
        runner.waiting_for_kind()
    );

    // Positive reach-guard: the same prompt still accepts the boundary X=max
    // and the activation completes with exact boundary payment.
    runner
        .act(GameAction::ChooseX { value: max })
        .expect("announcing the boundary X=max must succeed after the rejection");
    let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the damage mode must prompt for its target, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0, "CR 601.2c: the activator chooses the target");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("targeting P1 must succeed");
    drain_payment_to_priority(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the modal ability is on the stack after the legal announcement"
    );
    assert_eq!(pool_total(&runner, P0), 4, "pool after payment");
    assert_eq!(
        runner.state().objects[&marath]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "the counter leg removes exactly X=max (CR 107.3i reach-guard; \
         zero-count counters are dropped from the map)"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 20, "P0 untouched");
    assert_eq!(runner.life(P1), 14, "X=max=6 deals exactly 6 (CR 107.3a)");
}
