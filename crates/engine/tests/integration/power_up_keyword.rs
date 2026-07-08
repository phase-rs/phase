//! Runtime pipeline tests for the Power-up keyword mechanic (MSH/MSC).
//!
//! Power-up is a keyword-labeled activated ability (like Exhaust): `Power-up —
//! {cost}: {effect}`, activatable only once per game, with its activation cost's
//! generic reduced by the source's mana value if it entered this turn. These
//! tests drive the real parse→activation pipeline (`add_creature_from_oracle`
//! re-parses the Oracle text with the new parser; `GameRunner` drives `apply()`),
//! and each carries a revert-failing assertion (not an AST-shape assertion).
//!
//! CR 602.5b (verified docs/MagicCompRules.txt:2541): activate only once.
//! CR 602.2b + CR 601.2f + CR 302.6: cost reduced by source MV if it entered
//! this turn.
//! CR 106.6: tag-scoped mana spend restriction (Quinjet).
//! CR 500.7 + CR 514.2: Kang's extra-turn power-up prohibition.
//! CR 603.1b: Marvel Boy's dual-condition trigger.

use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::mana::{ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit};
use engine::types::phase::Phase;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

/// Locate the index of the (only) power-up-tagged ability on `object`.
fn power_up_index(runner: &GameRunner, object: ObjectId) -> usize {
    runner.state().objects[&object]
        .abilities
        .iter()
        .position(|a| a.ability_tag == Some(AbilityTag::PowerUp))
        .expect("object must have a power-up-tagged ability (parser produced the tag)")
}

fn p1p1_on(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Replace `player`'s mana pool with `n` colorless units (no restrictions).
fn refill_colorless(runner: &mut GameRunner, player: PlayerId, n: usize) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    pool.clear();
    for _ in 0..n {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

// ---------------------------------------------------------------------------
// 1. Parses as a tagged activated ability + once-per-game enforcement.
// ---------------------------------------------------------------------------

/// CR 602.5b: a power-up ability may be activated only once per GAME. Activate a
/// power-up, then attempt it again on a later turn → illegal.
///
/// Revert-failing assertion: the second `ActivateAbility` returns `Err`, and
/// `activated_abilities_this_game` stays at 1. If the `OnlyOnce` push the parser
/// adds is reverted, the ability is unrestricted and the second activation
/// succeeds. (Per-turn `OnlyOnceEachTurn` would reset across turns; this is per
/// game, so the later-turn retry is still rejected.)
#[test]
fn power_up_activates_only_once_per_game() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hero = scenario
        .add_creature_from_oracle(
            P0,
            "Power Hero",
            3,
            3,
            "Power-up — {2}: Put two +1/+1 counters on Power Hero.",
        )
        .id();

    let mut runner = scenario.build();
    let idx = power_up_index(&runner, hero);

    refill_colorless(&mut runner, P0, 6);
    runner.activate(hero, idx).resolve();
    assert!(
        p1p1_on(&runner, hero) >= 2,
        "first power-up activation must land two +1/+1 counters"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_game
            .get(&(hero, idx))
            .copied(),
        Some(1),
        "the per-game activation counter must record exactly one activation"
    );

    // Advance through to a later turn (passing priority crosses turn boundaries),
    // so a per-turn limit would have reset.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_to_phase(Phase::PreCombatMain);
    refill_colorless(&mut runner, P0, 6);

    let retry = runner.act(GameAction::ActivateAbility {
        source_id: hero,
        ability_index: idx,
    });
    assert!(
        retry.is_err(),
        "a second power-up activation on a later turn must be illegal (once per game)"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_game
            .get(&(hero, idx))
            .copied(),
        Some(1),
        "the per-game counter must remain 1 after the rejected second activation"
    );
}

/// Sibling/negative: a plain `Activate only once each turn` ability (no power-up
/// tag) DOES reset across turns. Proves the per-game limit is specific to the
/// `OnlyOnce` restriction the power-up parser pushes, not a global change.
#[test]
fn once_each_turn_ability_resets_across_turns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let pinger = scenario
        .add_creature_from_oracle(
            P0,
            "Each-Turn Pinger",
            2,
            2,
            "{2}: Put a +1/+1 counter on Each-Turn Pinger. Activate only once each turn.",
        )
        .id();

    let mut runner = scenario.build();
    let idx = pinger_index(&runner, pinger);

    refill_colorless(&mut runner, P0, 4);
    runner.activate(pinger, idx).resolve();
    let after_first = p1p1_on(&runner, pinger);
    assert!(after_first >= 1, "first activation lands a counter");

    // A second activation on the SAME turn is rejected (once each turn).
    refill_colorless(&mut runner, P0, 4);
    let same_turn = runner.act(GameAction::ActivateAbility {
        source_id: pinger,
        ability_index: idx,
    });
    assert!(
        same_turn.is_err(),
        "a once-each-turn ability cannot be activated twice in the same turn"
    );

    // Simulate the per-turn reset that `start_next_turn` performs at a turn
    // change (clearing `activated_abilities_this_turn`). P0 remains active and
    // holds priority, so the retry exercises only the per-turn-limit reset — a
    // per-game `OnlyOnce` ability would still be rejected after this reset.
    runner.state_mut().activated_abilities_this_turn.clear();
    refill_colorless(&mut runner, P0, 4);

    let retry = runner.act(GameAction::ActivateAbility {
        source_id: pinger,
        ability_index: idx,
    });
    assert!(
        retry.is_ok(),
        "a once-each-turn ability must be activatable again after the per-turn counter resets"
    );
}

fn pinger_index(runner: &GameRunner, object: ObjectId) -> usize {
    use engine::types::ability::ActivationRestriction;
    runner.state().objects[&object]
        .abilities
        .iter()
        .position(|a| {
            a.activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn)
        })
        .expect("must have a once-each-turn ability")
}

// ---------------------------------------------------------------------------
// 2. Entered-this-turn cost reduction (M3) — reduce generic by source MV.
// ---------------------------------------------------------------------------

/// CR 602.2b + CR 601.2f + CR 302.6: the power-up cost's generic is reduced by
/// the source's mana value when it entered this turn. A creature with printed
/// mana value 3 reduces a `{3}` power-up to `{0}` the entering turn.
///
/// Revert-failing assertion: with ZERO mana funded, the activation SUCCEEDS the
/// entering turn (cost reduced to {0}) and lands its counters. If the
/// `cost_reduction` field the parser sets is reverted, {3} is not reduced and
/// the zero-mana activation fails.
#[test]
fn power_up_cost_reduced_by_source_mana_value_when_entered_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Printed mana value 3 ({1}{R}{R}); power-up costs {3}.
    let hulk = scenario
        .add_creature_from_oracle(
            P0,
            "Mini Hulk",
            4,
            4,
            "Power-up — {3}: Put three +1/+1 counters on Mini Hulk.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    // Mark the creature as having entered THIS turn so the reduction gate holds.
    let this_turn = runner.state().turn_number;
    runner
        .state_mut()
        .objects
        .get_mut(&hulk)
        .unwrap()
        .entered_battlefield_turn = Some(this_turn);

    let idx = power_up_index(&runner, hulk);
    // Fund ZERO mana — the activation can only succeed if {3} is reduced to {0}.
    runner.state_mut().players[P0.0 as usize].mana_pool.clear();

    runner.activate(hulk, idx).resolve();
    assert!(
        p1p1_on(&runner, hulk) >= 3,
        "power-up entering this turn must be free (cost reduced by MV 3 → {{0}}); \
         the zero-mana activation must land its three counters"
    );
}

/// Negative: a power-up source that entered a PRIOR turn gets NO reduction, so a
/// zero-mana activation of a `{3}` power-up is rejected. Proves the reduction is
/// gated on `SourceEnteredThisTurn`, not unconditional.
#[test]
fn power_up_cost_not_reduced_when_entered_prior_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hulk = scenario
        .add_creature_from_oracle(
            P0,
            "Old Hulk",
            4,
            4,
            "Power-up — {3}: Put three +1/+1 counters on Old Hulk.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    // `add_creature` already set entered_battlefield_turn to a PRIOR turn; with
    // zero mana the unreduced {3} cost cannot be paid.
    runner.state_mut().players[P0.0 as usize].mana_pool.clear();
    let idx = power_up_index(&runner, hulk);

    let attempt = runner.act(GameAction::ActivateAbility {
        source_id: hulk,
        ability_index: idx,
    });
    assert!(
        attempt.is_err(),
        "a power-up that entered a prior turn gets no reduction; a zero-mana {{3}} \
         activation must be rejected"
    );
    assert_eq!(
        p1p1_on(&runner, hulk),
        0,
        "no counters land when the unreduced power-up cost is unaffordable"
    );
}

// ---------------------------------------------------------------------------
// 3. Hulk static — reduces OTHER controlled power-up abilities by {3}.
// ---------------------------------------------------------------------------

/// CR 601.2f: Hulk's static reduces the power-up activation cost of OTHER
/// creatures you control by {3}; it does NOT reduce Hulk's own (the "other"
/// self-exclusion).
///
/// Revert-failing assertion: with exactly {3} funded, the OTHER creature's `{6}`
/// power-up (reduced to {3}) is affordable and lands counters; remove Hulk and
/// the same {3} no longer covers the unreduced {6}. The "Hulk's own unaffected"
/// sibling is asserted by funding only {3} for Hulk's own {6} power-up → rejected.
#[test]
fn hulk_static_reduces_other_power_up_abilities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hulk = scenario
        .add_creature_from_oracle(
            P0,
            "Gamma Hulk",
            5,
            5,
            "Power-up abilities of other creatures you control cost {3} less to activate.\n\
             Power-up — {6}: Put five +1/+1 counters on Gamma Hulk.",
        )
        .id();
    let ally = scenario
        .add_creature_from_oracle(
            P0,
            "Ally Hero",
            2,
            2,
            "Power-up — {6}: Put two +1/+1 counters on Ally Hero.",
        )
        .id();

    let mut runner = scenario.build();
    let ally_idx = power_up_index(&runner, ally);

    // Fund exactly {3}: only affordable if Hulk's static reduces the ally's {6}
    // power-up by 3.
    refill_colorless(&mut runner, P0, 3);
    runner.activate(ally, ally_idx).resolve();
    assert!(
        p1p1_on(&runner, ally) >= 2,
        "Hulk's static must reduce the ally's {{6}} power-up to {{3}}, making it affordable"
    );

    // Hulk's OWN power-up is NOT reduced ("other"): with only {3}, the {6} is
    // unaffordable.
    let hulk_idx = power_up_index(&runner, hulk);
    refill_colorless(&mut runner, P0, 3);
    let own = runner.act(GameAction::ActivateAbility {
        source_id: hulk,
        ability_index: hulk_idx,
    });
    assert!(
        own.is_err(),
        "Hulk's static excludes itself (\"other\"); its own {{6}} power-up is not reduced"
    );
}

/// Revert anchor for the static: with NO Hulk on the battlefield, the ally's
/// {6} power-up is unaffordable at {3}. Proves the reduction comes from the
/// static, not a baseline change.
#[test]
fn power_up_without_hulk_static_is_not_reduced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ally = scenario
        .add_creature_from_oracle(
            P0,
            "Lone Hero",
            2,
            2,
            "Power-up — {6}: Put two +1/+1 counters on Lone Hero.",
        )
        .id();

    let mut runner = scenario.build();
    let idx = power_up_index(&runner, ally);
    refill_colorless(&mut runner, P0, 3);

    let attempt = runner.act(GameAction::ActivateAbility {
        source_id: ally,
        ability_index: idx,
    });
    assert!(
        attempt.is_err(),
        "without Hulk's static, the {{6}} power-up is not reduced and {{3}} is insufficient"
    );
}

// ---------------------------------------------------------------------------
// 4. Wonder Man static — raises the per-game power-up limit to 2.
// ---------------------------------------------------------------------------

/// CR 602.5b: Wonder Man's static lets each power-up ability of
/// permanents you control be activated an ADDITIONAL time (per game cap 2).
///
/// Revert-failing assertion: with Wonder Man present, the ally activates its
/// power-up TWICE; the third is illegal. Without the `effective_activation_limit_per_game`
/// change (the `OnlyOnce` arm reverting to `== 0`), the second activation would
/// already be rejected.
#[test]
fn wonder_man_static_raises_power_up_limit_to_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _wonder = scenario
        .add_creature_from_oracle(
            P0,
            "Wonder Hero",
            4,
            4,
            "Each power-up ability of permanents you control can be activated an additional time.",
        )
        .id();
    let ally = scenario
        .add_creature_from_oracle(
            P0,
            "Buddy Hero",
            2,
            2,
            "Power-up — {1}: Put a +1/+1 counter on Buddy Hero.",
        )
        .id();

    let mut runner = scenario.build();
    let idx = power_up_index(&runner, ally);

    refill_colorless(&mut runner, P0, 2);
    runner.activate(ally, idx).resolve();
    refill_colorless(&mut runner, P0, 2);
    // Second activation is legal because Wonder Man raised the cap to 2.
    let second = runner.act(GameAction::ActivateAbility {
        source_id: ally,
        ability_index: idx,
    });
    assert!(
        second.is_ok(),
        "Wonder Man raises the per-game power-up cap to 2; the second activation must be legal"
    );
    runner.advance_until_stack_empty();

    refill_colorless(&mut runner, P0, 2);
    // Third activation exceeds the cap of 2.
    let third = runner.act(GameAction::ActivateAbility {
        source_id: ally,
        ability_index: idx,
    });
    assert!(
        third.is_err(),
        "the third power-up activation exceeds the raised cap of 2"
    );
}

/// Negative: Wonder Man's static keys on the "power-up" tag, so it does NOT
/// raise the limit of a non-power-up `OnlyOnce` ability.
#[test]
fn wonder_man_does_not_raise_non_power_up_once_limit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _wonder = scenario
        .add_creature_from_oracle(
            P0,
            "Wonder Hero",
            4,
            4,
            "Each power-up ability of permanents you control can be activated an additional time.",
        )
        .id();
    // A non-power-up "activate only once" ability (e.g. an Exhaust-style line).
    let other = scenario
        .add_creature_from_oracle(
            P0,
            "Exhaust Hero",
            2,
            2,
            "Exhaust — {1}: Put a +1/+1 counter on Exhaust Hero.",
        )
        .id();

    let mut runner = scenario.build();
    use engine::types::ability::ActivationRestriction;
    let idx = runner.state().objects[&other]
        .abilities
        .iter()
        .position(|a| {
            a.ability_tag == Some(AbilityTag::Exhaust)
                && a.activation_restrictions
                    .contains(&ActivationRestriction::OnlyOnce)
        })
        .expect("must have an exhaust OnlyOnce ability");

    refill_colorless(&mut runner, P0, 2);
    runner.activate(other, idx).resolve();
    refill_colorless(&mut runner, P0, 2);

    let second = runner.act(GameAction::ActivateAbility {
        source_id: other,
        ability_index: idx,
    });
    assert!(
        second.is_err(),
        "Wonder Man's power-up-tagged static must not raise an exhaust OnlyOnce limit"
    );
}

// ---------------------------------------------------------------------------
// 5. Quinjet mana restriction (M4/M5) — tag-scoped spend gate.
// ---------------------------------------------------------------------------

/// CR 106.6: mana with `OnlyForTaggedActivation(PowerUp)` pays a power-up
/// activation but is rejected for a non-power-up activation. This drives the
/// REAL activation pipeline (the threaded `PaymentContext::Activation.ability_tag`),
/// not just `restriction.allows(...)`.
///
/// Revert-failing assertion: with ONLY tagged mana in the pool, the power-up
/// activation succeeds (mana consumed, counter lands) but the non-power-up
/// activation is rejected. If the tag threading through `pay_ability_mana_cost*`
/// is reverted, the context tag is lost and the restricted mana would (wrongly)
/// pay the non-power-up activation too.
#[test]
fn tagged_mana_pays_power_up_but_not_other_activations() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hero = scenario
        .add_creature_from_oracle(
            P0,
            "Restricted Hero",
            2,
            2,
            "Power-up — {1}: Put a +1/+1 counter on Restricted Hero.",
        )
        .id();
    // A non-power-up activated ability funded by the same restricted mana.
    let other = scenario
        .add_creature_from_oracle(
            P0,
            "Plain Pinger",
            2,
            2,
            "{1}: Put a +1/+1 counter on Plain Pinger.",
        )
        .id();

    let mut runner = scenario.build();
    let power_idx = power_up_index(&runner, hero);

    // The non-power-up activated ability index (no tag).
    let other_idx = runner.state().objects[&other]
        .abilities
        .iter()
        .position(|a| a.ability_tag.is_none() && a.cost.is_some())
        .expect("plain pinger must have an untagged activated ability");

    // Try to pay the non-power-up activation with ONLY power-up-restricted mana →
    // rejected (the {R}{R} can't pay it).
    set_tagged_mana(&mut runner, P0, 1);
    let other_attempt = runner.act(GameAction::ActivateAbility {
        source_id: other,
        ability_index: other_idx,
    });
    assert!(
        other_attempt.is_err(),
        "power-up-restricted mana must not pay a non-power-up activation"
    );

    // The same restricted mana DOES pay the power-up activation.
    set_tagged_mana(&mut runner, P0, 1);
    runner.activate(hero, power_idx).resolve();
    assert!(
        p1p1_on(&runner, hero) >= 1,
        "power-up-restricted mana must pay the power-up activation and land a counter"
    );
}

fn set_tagged_mana(runner: &mut GameRunner, player: PlayerId, n: usize) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    pool.clear();
    for _ in 0..n {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![ManaRestriction::OnlyForTaggedActivation(
                AbilityTag::PowerUp,
            )],
        ));
    }
}

// ---------------------------------------------------------------------------
// 6. Marvel Boy trigger — fires on power-up activation only.
// ---------------------------------------------------------------------------

/// CR 602.1 + CR 603.1b: "whenever you activate a power-up ability" triggers.
///
/// Revert-failing assertion: activating a power-up ability puts a +1/+1 counter
/// on Marvel Boy; activating a NON-power-up ability does not. If the trigger arm
/// is reverted, the parser drops the condition and no counter ever lands.
#[test]
fn marvel_boy_triggers_only_on_power_up_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let marvel = scenario
        .add_creature_from_oracle(
            P0,
            "Marvel Boy",
            2,
            2,
            "Whenever you activate a power-up ability, put a +1/+1 counter on Marvel Boy.",
        )
        .id();
    let hero = scenario
        .add_creature_from_oracle(
            P0,
            "Counter Hero",
            2,
            2,
            "Power-up — {1}: Put a +1/+1 counter on Counter Hero.",
        )
        .id();
    let plain = scenario
        .add_creature_from_oracle(
            P0,
            "Plain Hero",
            2,
            2,
            "{1}: Put a +1/+1 counter on Plain Hero.",
        )
        .id();

    let mut runner = scenario.build();

    let before = p1p1_on(&runner, marvel);

    // Activate the NON-power-up ability first: must NOT trigger Marvel Boy.
    let plain_idx = runner.state().objects[&plain]
        .abilities
        .iter()
        .position(|a| a.ability_tag.is_none() && a.cost.is_some())
        .expect("plain hero has an untagged activated ability");
    refill_colorless(&mut runner, P0, 1);
    runner.activate(plain, plain_idx).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        p1p1_on(&runner, marvel),
        before,
        "a non-power-up activation must NOT trigger Marvel Boy"
    );

    // Activate the power-up ability: Marvel Boy gains a counter.
    let power_idx = power_up_index(&runner, hero);
    refill_colorless(&mut runner, P0, 1);
    runner.activate(hero, power_idx).resolve();
    runner.advance_until_stack_empty();
    assert!(
        p1p1_on(&runner, marvel) > before,
        "a power-up activation must trigger Marvel Boy (+1/+1 counter)"
    );
}

// ---------------------------------------------------------------------------
// 7. Kang prohibition (B1/B2) — blocks power-up throughout the extra turn.
// ---------------------------------------------------------------------------

/// Drive the real apply()/turn pipeline forward until the active player reaches
/// `Phase::PreCombatMain` of turn number `target_turn`, declaring no
/// attackers/blockers when combat opens. Crossing turns this way runs the
/// untap-step arming (turns.rs converts `UntilEndOfNextTurnOf` → `EndOfTurn`)
/// and the cleanup-step prune exactly as in production.
fn advance_to_main_of_turn(runner: &mut GameRunner, target_turn: u32) {
    for _ in 0..400 {
        if runner.state().turn_number >= target_turn
            && runner.state().phase == Phase::PreCombatMain
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers while advancing turns");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers while advancing turns");
            }
            other => panic!(
                "unexpected prompt advancing to turn {target_turn}: {other:?} \
                 (phase={:?}, turn={})",
                runner.state().phase,
                runner.state().turn_number,
            ),
        }
    }
    panic!(
        "failed to reach PreCombatMain of turn {target_turn} (now turn {}, phase {:?})",
        runner.state().turn_number,
        runner.state().phase,
    );
}

/// CR 500.7 + CR 514.2 + CR 602.5: Kang's "during that turn, power-up abilities
/// can't be activated" scopes the prohibition to the GRANTED EXTRA TURN ONLY. The
/// restriction is created pre-armed (`UntilEndOfNextTurnOf`) when Kang resolves on
/// turn N, must stay DORMANT for the rest of turn N, arms (converts to
/// `EndOfTurn`) at the extra turn's untap step, blocks power-up throughout that
/// turn, then is pruned at the extra turn's cleanup.
///
/// This drives the full resolve→extra-turn→post-extra-turn pipeline. Discriminating
/// (revert-failing) assertion: on the CREATION turn a controller-side power-up
/// ability is STILL LEGAL — under the buggy "active on creation turn" reading
/// (expiry discarded in `is_blocked_by_cant_activate_abilities`) it would be
/// wrongly rejected. The extra-turn block and post-extra-turn expiry confirm the
/// arming/pruning lifecycle.
#[test]
fn kang_prohibits_power_up_during_extra_turn_only() {
    use engine::types::ability::{GameRestriction, ProhibitedActivity, RestrictionExpiry};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kang = scenario
        .add_creature_from_oracle(
            P0,
            "Kang",
            3,
            3,
            "Power-up — {1}: Put a +1/+1 counter on Kang. Take an extra turn after this one. \
             During that turn, power-up abilities can't be activated.",
        )
        .id();
    // `hero` exercises the creation-turn dormancy (activated on turn N); `hero2`
    // is reserved (never consumed) so the extra-turn block and post-expiry
    // legality are caused by the prohibition alone, not the once-per-game limit.
    let hero = scenario
        .add_creature_from_oracle(
            P0,
            "Extra Hero",
            2,
            2,
            "Power-up — {1}: Put a +1/+1 counter on Extra Hero.",
        )
        .id();
    let hero2 = scenario
        .add_creature_from_oracle(
            P0,
            "Reserve Hero",
            2,
            2,
            "Power-up — {1}: Put a +1/+1 counter on Reserve Hero.",
        )
        .id();
    // Untagged ability — must stay legal even while the power-up prohibition is
    // armed (only_tag scoping).
    let plain = scenario
        .add_creature_from_oracle(
            P0,
            "Plain Hero",
            2,
            2,
            "{1}: Put a +1/+1 counter on Plain Hero.",
        )
        .id();

    // CR 104.3c / 704.5c: stock both libraries so neither player decks out (and
    // ends the game) while this test advances P0 through Kang's granted extra turn
    // and on to P0's following turn — the multi-turn advance crosses several draw
    // steps, which would otherwise trigger a draw-from-empty loss before the
    // post-expiry assertion is reached.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Forest");
        scenario.add_card_to_library_top(P1, "Forest");
    }

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().active_player,
        P0,
        "Kang's controller must be the active player for the extra-turn test"
    );
    let kang_idx = power_up_index(&runner, kang);

    refill_colorless(&mut runner, P0, 1);
    runner.activate(kang, kang_idx).resolve();
    runner.advance_until_stack_empty();

    let creation_turn = runner.state().turn_number;

    // Kang granted P0 an extra turn and added a power-up-scoped, pre-armed
    // prohibition.
    assert!(
        runner.state().extra_turns.contains(&P0),
        "Kang must grant its controller an extra turn"
    );
    let prohibition_is_prearmed = runner.state().restrictions.iter().any(|r| {
        matches!(
            r,
            GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::ActivateAbilities {
                    only_tag: Some(AbilityTag::PowerUp),
                    ..
                },
                expiry: RestrictionExpiry::UntilEndOfNextTurnOf { .. },
                ..
            }
        )
    });
    assert!(
        prohibition_is_prearmed,
        "Kang must add a power-up-scoped prohibition that is pre-armed \
         (UntilEndOfNextTurnOf) on the creation turn"
    );

    // === Step 1 (revert-failing): on the creation turn the prohibition is
    // DORMANT, so a controller-side power-up ability is STILL LEGAL. The buggy
    // reading (expiry discarded) would reject this.
    let hero_idx = power_up_index(&runner, hero);
    refill_colorless(&mut runner, P0, 1);
    let creation_turn_attempt = runner.act(GameAction::ActivateAbility {
        source_id: hero,
        ability_index: hero_idx,
    });
    assert!(
        creation_turn_attempt.is_ok(),
        "power-up must remain legal on the CREATION turn — Kang's prohibition is \
         scoped to the extra turn only (pre-armed, not yet in force)"
    );
    runner.advance_until_stack_empty();

    // === Step 2: enter the granted extra turn (N+1). Its untap step arms the
    // prohibition (converts to EndOfTurn); power-up is now BLOCKED throughout.
    advance_to_main_of_turn(&mut runner, creation_turn + 1);
    assert_eq!(
        runner.state().active_player,
        P0,
        "the turn after Kang's must be P0's granted extra turn"
    );
    let hero2_idx = power_up_index(&runner, hero2);
    refill_colorless(&mut runner, P0, 1);
    let extra_turn_attempt = runner.act(GameAction::ActivateAbility {
        source_id: hero2,
        ability_index: hero2_idx,
    });
    assert!(
        extra_turn_attempt.is_err(),
        "power-up must be blocked during the granted extra turn (prohibition armed)"
    );

    // Sibling/negative: an untagged ability is unaffected by the power-up-only
    // prohibition, even while it is armed during the extra turn.
    let plain_idx = runner.state().objects[&plain]
        .abilities
        .iter()
        .position(|a| a.ability_tag.is_none() && a.cost.is_some())
        .expect("plain hero has an untagged ability");
    refill_colorless(&mut runner, P0, 1);
    let plain_attempt = runner.act(GameAction::ActivateAbility {
        source_id: plain,
        ability_index: plain_idx,
    });
    assert!(
        plain_attempt.is_ok(),
        "a non-power-up activation must remain legal under a power-up-only prohibition"
    );
    runner.advance_until_stack_empty();

    // === Step 3: the prohibition is pruned at the extra turn's cleanup. By P0's
    // next turn (N+3) it has expired and power-up is legal again.
    advance_to_main_of_turn(&mut runner, creation_turn + 3);
    assert_eq!(
        runner.state().active_player,
        P0,
        "should be back on a P0 turn two turns after the extra turn"
    );
    let prohibition_remains = runner.state().restrictions.iter().any(|r| {
        matches!(
            r,
            GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::ActivateAbilities {
                    only_tag: Some(AbilityTag::PowerUp),
                    ..
                },
                ..
            }
        )
    });
    assert!(
        !prohibition_remains,
        "the power-up prohibition must be pruned at the extra turn's cleanup"
    );
    refill_colorless(&mut runner, P0, 1);
    let post_expiry_attempt = runner.act(GameAction::ActivateAbility {
        source_id: hero2,
        ability_index: hero2_idx,
    });
    assert!(
        post_expiry_attempt.is_ok(),
        "power-up must be legal again once Kang's prohibition has expired"
    );
}

// ---------------------------------------------------------------------------
// 8. Kang skipped-extra-turn arming-leak edge (FIX 3) — documented known leak.
// ---------------------------------------------------------------------------

/// FIX 3 (documented narrow edge): the arming conversion fires only at the
/// granted turn's untap step. If that turn is SKIPPED before its untap step, the
/// `UntilEndOfNextTurnOf` restriction is never converted to `EndOfTurn` and never
/// pruned — it wrongly persists. This test records the KNOWN LEAK as a regression
/// anchor for a future shared robust-arming fix (it asserts the leak EXISTS, so
/// it will start failing once the edge is fixed, prompting an update).
#[test]
fn kang_skipped_extra_turn_leaks_prohibition_known_edge() {
    use engine::types::ability::{
        GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
    };
    use engine::types::statics::ActivationExemption;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _filler = scenario.add_creature(P0, "Filler", 1, 1).id();
    let mut runner = scenario.build();

    // Inject the pre-armed prohibition keyed to P0, as Kang's lowering would after
    // granting P0 an extra turn. The arming conversion only fires at P0's untap
    // step (when `player == active`). If the granted P0 turn is skipped/prevented,
    // that untap never runs, so the restriction is never converted/pruned.
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: ObjectId(0),
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::UntilEndOfNextTurnOf { player: P0 },
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::None,
                only_tag: Some(AbilityTag::PowerUp),
            },
        });

    // Simulate the OPPONENT's (P1's) untap step running while the restriction is
    // keyed to P0 — exactly the "skipped P0 extra turn" condition (no P0 untap
    // runs). The untap pre-pass converts only matching `player == active`, so a
    // P1 untap must NOT arm P0's restriction.
    runner.state_mut().active_player = P1;
    let mut events = Vec::new();
    engine::game::turns::execute_untap_with_choices(
        runner.state_mut(),
        &mut events,
        &std::collections::HashSet::new(),
    );

    // KNOWN LEAK: the restriction is still present, un-armed (still
    // `UntilEndOfNextTurnOf`, not converted to `EndOfTurn`), because no untap step
    // matching `player: P0` ran.
    let still_present = runner.state().restrictions.iter().any(|r| {
        matches!(
            r,
            GameRestriction::ProhibitActivity {
                expiry: RestrictionExpiry::UntilEndOfNextTurnOf { player },
                ..
            } if *player == P0
        )
    });
    assert!(
        still_present,
        "documented edge: when the granted extra turn's untap never runs, the \
         un-armed prohibition persists (remove this assertion once robust arming \
         is implemented and convert it to assert the restriction is gone)"
    );
}
