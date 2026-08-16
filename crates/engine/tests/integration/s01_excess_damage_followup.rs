//! S01 excess-damage "this way" follow-up channel — CARD-SPECIFIC runtime
//! coverage for Torch the Witness and Orbital Plunge.
//!
//! Both cards deal damage to a target creature and then, in the SAME resolution,
//! run a follow-up leg gated on "if excess damage was dealt [to a permanent]
//! this way" (CR 120.10). The resolver-level class test
//! `deal_damage_excess_channel_sums_excess_not_total_and_gates_followup`
//! (deal_damage.rs) proves the channel machinery with a synthetic mana follow-up;
//! these tests drive each card's REAL Oracle text through a full cast so the
//! parser lowering (`PreviousEffectAmount { channel: Excess }`) AND the card's
//! real follow-up (Investigate → a Clue token; the Lander token) both resolve.
//!
//! Discriminating axis (per card): an OVERKILL target (excess > 0) must fire the
//! follow-up; an EXACT-LETHAL target (excess == 0) must NOT. If the parser had
//! mis-lowered the channel to `Total`, the exact-lethal leg would WRONGLY fire
//! (total damage > 0), so the exact-lethal assertion is the revert-failing one.
//!
//! Oracle text source: `data/mtgish-cards.json` (Torch the Witness /
//! Orbital Plunge Rules trees decode to the passive-voice excess-damage
//! condition the parser doc-comments in `oracle_effect/conditions.rs` cite by
//! name). Printed wording confirmed against the cards as printed.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TORCH_ORACLE: &str = "Torch the Witness deals twice X damage to target creature. \
If excess damage was dealt to a permanent this way, investigate.";

const ORBITAL_ORACLE: &str = "Orbital Plunge deals 6 damage to target creature. \
If excess damage was dealt this way, create a Lander token.";

/// Count the artifact tokens P0 controls whose subtype list contains `subtype`
/// (the follow-up's product: "Clue" for investigate, "Lander" for Orbital).
fn token_count_by_subtype(runner: &GameRunner, subtype: &str) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.controller == P0
                && o.zone == Zone::Battlefield
                && o.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .count()
}

/// Fill P0's pool with `n` red units (red pays both the colored pip and the
/// generic/X portion of these sorceries) so `CastPaymentMode::Auto` never stalls
/// in a `ManaPayment` prompt.
fn fill_red_pool(scenario: &mut GameScenario, n: usize) {
    let mana: Vec<ManaUnit> = (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);
}

/// Torch the Witness: {X}{R} — "twice X" damage. Vary X to control excess against
/// a fixed 2-toughness target. Returns the number of Clue tokens P0 controls once
/// the spell has fully resolved.
fn cast_torch_and_count_clues(x: u32, target_toughness: i32) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario
        .add_creature(engine::game::scenario::P1, "Witness", 1, target_toughness)
        .id();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Torch the Witness", false, TORCH_ORACLE);
    // {X}{R}
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Red],
        generic: 0,
    });
    let spell = builder.id();

    fill_red_pool(&mut scenario, 12);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![target],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Torch the Witness must be accepted");

    drive_x_and_resolve(&mut runner, Some(x), target);
    token_count_by_subtype(&runner, "Clue")
}

/// Orbital Plunge: {3}{R} — fixed 6 damage. Vary the target's toughness to
/// control excess. Returns the number of Lander tokens P0 controls once resolved.
fn cast_orbital_and_count_landers(target_toughness: i32) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario
        .add_creature(
            engine::game::scenario::P1,
            "Landfall Beast",
            1,
            target_toughness,
        )
        .id();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Orbital Plunge", false, ORBITAL_ORACLE);
    // {3}{R}
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 3,
    });
    let spell = builder.id();

    fill_red_pool(&mut scenario, 12);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![target],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Orbital Plunge must be accepted");

    drive_x_and_resolve(&mut runner, None, target);
    token_count_by_subtype(&runner, "Lander")
}

/// Drive the cast pipeline: commit X (if the cost has one), satisfy any late
/// target prompt, then pass priority until the spell has fully resolved.
fn drive_x_and_resolve(runner: &mut GameRunner, x: Option<u32>, target: ObjectId) {
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                let value = x.expect("a spell without X must not surface ChooseXValue");
                runner
                    .act(GameAction::ChooseX { value })
                    .expect("committing X must succeed");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target)],
                    })
                    .expect("targeting the creature must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!("unexpected prompt while casting excess-damage spell: {other:?}"),
        }
    }
    panic!("cast pipeline did not settle within the prompt budget");
}

/// Torch the Witness — OVERKILL (X=3 → 6 damage to a 2-toughness creature, excess
/// 4): the "if excess damage was dealt to a permanent this way, investigate" leg
/// fires and a Clue token is created.
///
/// REVERT-PROBE: if the parser mis-lowered the excess condition to
/// `DamageChannel::Total`, this leg would still fire (total 6 > 0), so this
/// assertion alone is not discriminating — it is paired with the exact-lethal
/// test below, where a Total channel would WRONGLY fire.
#[test]
fn torch_the_witness_overkill_investigates() {
    assert_eq!(
        cast_torch_and_count_clues(3, 2),
        1,
        "excess damage (6-2=4 > 0) → Torch investigates, creating one Clue token"
    );
}

/// Torch the Witness — EXACT LETHAL (X=1 → 2 damage to a 2-toughness creature,
/// excess 0): no excess was dealt, so the investigate leg is declined and NO
/// Clue token is created.
///
/// REVERT-PROBE (the discriminating assertion): a `DamageChannel::Total`
/// mis-lowering would read total damage (2 > 0) and WRONGLY investigate here.
/// The `Excess` channel reads 0, so the leg correctly declines.
#[test]
fn torch_the_witness_exact_lethal_does_not_investigate() {
    assert_eq!(
        cast_torch_and_count_clues(1, 2),
        0,
        "zero excess (2-2=0) → Torch must NOT investigate; a Total-channel \
         mis-lowering would wrongly fire here (total 2 > 0)"
    );
}

/// Orbital Plunge — OVERKILL (6 damage to a 4-toughness creature, excess 2): the
/// "if excess damage was dealt this way, create a Lander token" leg fires.
#[test]
fn orbital_plunge_overkill_creates_lander() {
    assert_eq!(
        cast_orbital_and_count_landers(4),
        1,
        "excess damage (6-4=2 > 0) → Orbital Plunge creates one Lander token"
    );
}

/// Orbital Plunge — EXACT LETHAL (6 damage to a 6-toughness creature, excess 0):
/// no excess, so no Lander token.
///
/// REVERT-PROBE (the discriminating assertion): a `DamageChannel::Total`
/// mis-lowering would read total 6 > 0 and WRONGLY create a Lander here.
#[test]
fn orbital_plunge_exact_lethal_creates_no_lander() {
    assert_eq!(
        cast_orbital_and_count_landers(6),
        0,
        "zero excess (6-6=0) → Orbital Plunge must NOT create a Lander; a \
         Total-channel mis-lowering would wrongly fire here (total 6 > 0)"
    );
}
