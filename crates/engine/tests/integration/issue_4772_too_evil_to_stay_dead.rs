//! Issue #4772: Too Evil to Stay Dead did nothing when cast WITHOUT paying its
//! Teamwork cost — it spent the mana, let the caster choose a target, and then
//! never returned the creature card to the battlefield.
//!
//! Oracle text (verified against Scryfall, MSH):
//!   "Teamwork 4 (As an additional cost to cast this spell, you may tap any
//!   number of creatures you control with total power 4 or more.)
//!   Choose target creature card in your graveyard with mana value 4 or less.
//!   If this spell was cast using teamwork, instead choose target creature
//!   card in your graveyard. Return the chosen card to the battlefield."
//!
//! Root cause: this lowers to a 3-node chain — a base `TargetOnly` clause (the
//! narrow, mana-value-gated "choose target"), whose `sub_ability` is the
//! teamwork-gated override (`AbilityCondition::AdditionalCostPaidInstead`, the
//! broader "choose target" from `strip_additional_cost_conditional`), whose OWN
//! `sub_ability` is the trailing `Effect::ChangeZone` (`SubAbilityLink::
//! SequentialSibling`, no `else_ability`) that actually returns the card to the
//! battlefield. `resolve_ability_chain` in `crates/engine/src/game/effects/mod.rs`
//! correctly declines to swap the base's effect for the override's when Teamwork
//! was NOT paid, but — before this fix — only its `ConditionInstead` arm had a
//! fallback that walks into `sub.sub_ability` when `sub.else_ability` is `None`.
//! The `AdditionalCostPaidInstead` / `CastVariantPaidInstead` /
//! `TargetHasKeywordInstead` arm had no such fallback: it checked `else_ability`
//! and then unconditionally returned, so the not-swapped branch silently dropped
//! the `ChangeZone` reanimation effect. The fix merges the two arms so all four
//! "instead" condition kinds share the same not-swap tail-runner (mirroring the
//! existing `condition_instead_not_swap_tail_runner_honors_gates` coverage in
//! `crates/engine/src/game/effects/mod.rs`).
//!
//! This test pins BOTH branches:
//!   - WITHOUT teamwork: the mana-value-gated target is returned to the
//!     battlefield (was previously a complete no-op — the bug).
//!   - WITH teamwork: the broader (no mana-value restriction) target is
//!     returned to the battlefield (already worked before this fix, kept here
//!     as a differential control so a revert of either branch fails a test).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const TOO_EVIL_TO_STAY_DEAD: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose target creature card in your graveyard with mana value 4 or less. If this spell was cast using teamwork, instead choose target creature card in your graveyard. Return the chosen card to the battlefield.";

/// Build a scenario with Too Evil to Stay Dead in P0's hand (cost {0} so the
/// test doesn't need to model mana payment), a mana-value-3 creature card in
/// P0's graveyard (a legal target under BOTH the narrow and broad filters),
/// and (if `tapper_power` is `Some`) an eligible teamwork tap creature.
fn setup(tapper_power: Option<i32>) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut gy_creature = scenario.add_creature_to_graveyard(P0, "Fallen Champion", 3, 3);
    gy_creature.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 3,
    });
    let gy_creature_id = gy_creature.id();

    let tapper_id =
        tapper_power.map(|power| scenario.add_creature(P0, "Tapper", power, power).id());

    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Too Evil to Stay Dead",
        false,
        TOO_EVIL_TO_STAY_DEAD,
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();

    let runner = scenario.build();
    (runner, spell, gy_creature_id, tapper_id)
}

/// Cast `spell`, decline the optional Teamwork cost at the first opportunity,
/// and choose `target` at the first target-selection window.
fn drive_cast_declining_teamwork(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("declining teamwork must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing the target must be accepted");
            }
            WaitingFor::Priority { .. } => return,
            _ => return,
        }
    }
}

/// Cast `spell`, ACCEPT the optional Teamwork cost, tap `tapper` to pay the
/// aggregate power requirement, and choose `target` at the first
/// target-selection window.
fn drive_cast_paying_teamwork(runner: &mut GameRunner, tapper: ObjectId, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("paying teamwork must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tapper],
                    })
                    .expect("tapping the teamwork creature must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing the target must be accepted");
            }
            WaitingFor::Priority { .. } => return,
            _ => return,
        }
    }
}

/// Resolve the stack to empty by passing priority.
fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// Regression for issue #4772: WITHOUT paying Teamwork, the targeted graveyard
/// creature must still be returned to the battlefield. Before the fix, the
/// `ChangeZone` step was silently dropped and the creature stayed in the
/// graveyard — the spell "spent the mana with no effect".
#[test]
fn too_evil_to_stay_dead_without_teamwork_still_reanimates() {
    let (mut runner, spell, gy_creature, _tapper) = setup(None);
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Too Evil to Stay Dead must be accepted");

    drive_cast_declining_teamwork(&mut runner, gy_creature);
    resolve_stack(&mut runner);

    assert!(
        runner.state().battlefield.contains(&gy_creature),
        "WITHOUT teamwork, the targeted graveyard creature must be returned to \
         the battlefield — this is the exact issue #4772 symptom (spell resolved, \
         mana was spent, but nothing happened)"
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&gy_creature),
        "the reanimated creature must have left the graveyard"
    );
}

/// Differential control: WITH Teamwork paid, the broader (no mana-value
/// restriction) target is chosen and still returned to the battlefield. This
/// branch already worked before the fix (the Cow-swap correctly adopts the
/// override's own `sub_ability` as its continuation); it is pinned here
/// alongside the without-teamwork case so a revert of either the swap path or
/// the new not-swap tail-runner is caught.
#[test]
fn too_evil_to_stay_dead_with_teamwork_still_reanimates() {
    let (mut runner, spell, gy_creature, tapper) = setup(Some(4));
    let tapper = tapper.expect("tapper requested");
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Too Evil to Stay Dead must be accepted");

    drive_cast_paying_teamwork(&mut runner, tapper, gy_creature);
    resolve_stack(&mut runner);

    assert!(
        runner.state().objects[&tapper].tapped,
        "the teamwork tap creature must be tapped"
    );
    assert!(
        runner.state().battlefield.contains(&gy_creature),
        "WITH teamwork, the targeted graveyard creature must be returned to the battlefield"
    );
}
