//! Regression test for #3179 (accept path of reflexive "up to that many"
//! triggers). Swashbuckler Extraordinaire: "Whenever you attack, you may
//! sacrifice one or more Treasures. When you do, up to that many target
//! creatures gain double strike until end of turn."
//!
//! Bug (CLASS defect): the reflexive `PendingTrigger` was created with
//! `subject_match_count: None`, so the "up to that many" bound (an
//! `EventContextAmount` resolved against the sacrifice count) collapsed to 0 in
//! the fresh `apply()` at target-assign time. The ACCEPT path then either
//! hard-errored with "Unused selected target slots" (a target was chosen but
//! the bound's max was 0) or silently resolved granting double strike to 0
//! creatures.
//!
//! These tests drive the full pipeline (attack → accept optional sacrifice →
//! select Treasures → choose target creatures → resolve) and assert the chosen
//! creatures actually gain double strike. They FAIL before the creation-site
//! freeze fix (in `effects/mod.rs`) and PASS after.

use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::zones::Zone;
use engine::types::Phase;

const SWASH: &str = "Whenever you attack, you may sacrifice one or more Treasures. When you do, up to that many target creatures gain double strike until end of turn.";

/// Drives the reflexive flow. Sacrifices `n_sacrifice` Treasures, then chooses
/// targets for the first `n_targets` reflexive slots (skipping the remainder to
/// exercise up-to semantics). Returns the object ids chosen as targets, in the
/// order they were assigned, so the caller can assert which creatures got
/// double strike. Every `act()` is `expect`-ed so a regression (e.g. "Unused
/// selected target slots") fails the test rather than silently passing.
fn drive(
    runner: &mut engine::game::scenario::GameRunner,
    treasures: &[ObjectId],
    n_sacrifice: usize,
    n_targets: usize,
) -> Vec<ObjectId> {
    let mut chosen_targets: Vec<ObjectId> = Vec::new();
    let mut targets_chosen = 0usize;
    for _ in 0..60 {
        let wf = runner.state().waiting_for.clone();
        match wf {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional sacrifice");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                let pick: Vec<_> = treasures.iter().take(n_sacrifice).copied().collect();
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("select treasures to sacrifice");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let target = if targets_chosen < n_targets {
                    // CR 601.2c: the same creature can't be chosen for two
                    // instances of "target" in one up-to set — pick a legal
                    // target not already chosen.
                    target_slots[selection.current_slot]
                        .legal_targets
                        .iter()
                        .find(|t| match t {
                            engine::types::ability::TargetRef::Object(id) => {
                                !chosen_targets.contains(id)
                            }
                            _ => true,
                        })
                        .cloned()
                } else {
                    None // skip remaining optional slots (up-to semantics)
                };
                if let Some(engine::types::ability::TargetRef::Object(id)) = target {
                    chosen_targets.push(id);
                    targets_chosen += 1;
                }
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("choose trigger target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    chosen_targets
}

/// Builds the Swashbuckler scenario: the attacker, `n_treasures` Treasure
/// tokens for P0, and two P1 creatures as targets. Returns (runner, swash id,
/// treasure ids, [bear_a, bear_b]).
fn setup(
    n_treasures: usize,
) -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    Vec<ObjectId>,
    Vec<ObjectId>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swash = scenario
        .add_creature_from_oracle(P0, "Swashbuckler Extraordinaire", 2, 2, SWASH)
        .id();

    let mut treasures = vec![];
    for _ in 0..n_treasures {
        let t = scenario.add_creature(P0, "Treasure", 0, 0).id();
        treasures.push(t);
    }

    let bear_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P1, "Bear B", 2, 2).id();

    let mut runner = scenario.build();

    // Retype the bare artifacts as Treasure tokens.
    for &t in &treasures {
        let obj = runner.state_mut().objects.get_mut(&t).unwrap();
        obj.card_types.core_types.clear();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Treasure".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.is_token = true;
    }

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(swash, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declare attackers");

    (runner, swash, treasures, vec![bear_a, bear_b])
}

fn has_double_strike(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    object_has_effective_keyword_kind(runner.state(), id, KeywordKind::DoubleStrike)
}

/// Sacrifice 2 Treasures, choose 2 targets → exactly those 2 creatures gain
/// double strike. The fix makes the up-to bound resolve to 2; pre-fix the
/// second slot's assign hard-errors with "Unused selected target slots".
#[test]
fn accept_two_targets_grants_double_strike() {
    let (mut runner, _swash, treasures, bears) = setup(2);
    let chosen = drive(&mut runner, &treasures, 2, 2);

    assert_eq!(
        chosen.len(),
        2,
        "both target slots should have been offered and chosen"
    );

    // The two chosen creatures gained double strike...
    for &id in &chosen {
        assert!(
            has_double_strike(&runner, id),
            "chosen creature {id:?} should have double strike"
        );
    }
    // ...and no other battlefield creature did (only the two bears were chosen,
    // and they are the only non-attacker creatures).
    let ds_total = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.card_types.core_types.contains(&CoreType::Creature)
                && object_has_effective_keyword_kind(
                    runner.state(),
                    o.id,
                    KeywordKind::DoubleStrike,
                )
        })
        .count();
    assert_eq!(
        ds_total, 2,
        "exactly the two chosen creatures should have double strike"
    );
    // Both treasures were actually sacrificed (left the battlefield).
    for &t in &treasures {
        assert_ne!(
            runner.state().objects.get(&t).map(|o| o.zone),
            Some(Zone::Battlefield),
            "treasure {t:?} should have been sacrificed"
        );
    }
    assert_eq!(bears.len(), 2);
}

/// Up-to semantics: sacrifice 2, choose only 1 target → exactly 1 creature
/// gains double strike and resolution is clean (no "Unused selected target
/// slots"). The second slot is declined; the bound's max is 2 (the sacrifice
/// count) but the controller may choose fewer.
#[test]
fn accept_one_target_grants_one_double_strike() {
    let (mut runner, _swash, treasures, bears) = setup(2);
    let chosen = drive(&mut runner, &treasures, 2, 1);

    assert_eq!(
        chosen.len(),
        1,
        "exactly one target should have been chosen"
    );
    assert!(
        has_double_strike(&runner, chosen[0]),
        "the single chosen creature should have double strike"
    );

    let ds_total = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.card_types.core_types.contains(&CoreType::Creature)
                && object_has_effective_keyword_kind(
                    runner.state(),
                    o.id,
                    KeywordKind::DoubleStrike,
                )
        })
        .count();
    assert_eq!(
        ds_total, 1,
        "exactly one creature should have double strike under up-to semantics"
    );

    // The unchosen bear did NOT gain double strike.
    let unchosen = *bears.iter().find(|&&b| b != chosen[0]).unwrap();
    assert!(
        !has_double_strike(&runner, unchosen),
        "the unchosen creature must not have double strike"
    );
}
