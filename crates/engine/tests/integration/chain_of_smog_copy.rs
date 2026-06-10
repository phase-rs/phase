//! Integration tests for Chain of Smog's optional copy sub-ability
//! (GitHub issue #427).
//!
//! Oracle text:
//!   Target player discards two cards. That player may copy this spell and
//!   may choose a new target for that copy.
//!
//! Two defects were fixed:
//!   (1) Parser — the "and may choose a new target for that copy" clause was
//!       not recognized, so the `CopySpell` sub-ability parsed with
//!       `retarget: KeepOriginalTargets` instead of `MayChooseNewTargets`
//!       (CR 707.10c).
//!   (2) Engine — `copy_spell::resolve` assigned the copy's controller (and
//!       the "may copy" prompt + retarget choice) to the original spell's
//!       caster rather than the targeted player. CR 707.10: "A copy of a
//!       spell is controlled by the player under whose control it was put on
//!       the stack" — for "That player may copy", the targeted player.
//!
//! The runtime tests drive the real `apply` pipeline: cast → player-target
//! selection → discard → optional-copy decision → copy-on-stack → retarget.
//! The copy is never hand-constructed.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CopyRetargetPermission, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// The third player. `scenario` only exports `P0`/`P1`.
const P2: PlayerId = PlayerId(2);

use crate::support::shared_card_db as load_db;
use engine::types::game_state::CastPaymentMode;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn hand_size(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .hand
        .len()
}

// ---------------------------------------------------------------------------
// Parser: "and may choose a new target for that copy" must set the inner
// `CopySpell` sub-ability's retarget to `MayChooseNewTargets` (CR 707.10c).
// ---------------------------------------------------------------------------

#[test]
fn chain_of_smog_parses_copy_sub_ability_with_may_choose_new_targets() {
    use engine::parser::oracle::parse_oracle_text;

    let parsed = parse_oracle_text(
        "Target player discards two cards. That player may copy this spell and \
         may choose a new target for that copy.",
        "Chain of Smog",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    let ability = parsed
        .abilities
        .first()
        .expect("Chain of Smog has a spell ability");
    assert!(
        matches!(*ability.effect, Effect::Discard { .. }),
        "parent effect is the two-card discard, got {:?}",
        ability.effect
    );

    let copy = ability
        .sub_ability
        .as_ref()
        .expect("the optional copy is a sub-ability of the discard");
    assert!(
        copy.optional,
        "the copy sub-ability is optional (\"may copy\")"
    );
    assert!(
        matches!(
            *copy.effect,
            Effect::CopySpell {
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                ..
            }
        ),
        "\"may choose a new target for that copy\" must set MayChooseNewTargets, \
         got {:?}",
        copy.effect
    );
}

/// CR 707.10c: the same clause must be recognized for the rest of the Chain
/// cycle (Chain of Acid / Plasma / Vapor share the "may copy this spell and
/// may choose a new target for that copy" template), so the parser fix is a
/// class fix, not a Chain-of-Smog special case.
#[test]
fn chain_cycle_copy_clause_sets_may_choose_new_targets_for_the_class() {
    use engine::parser::oracle::parse_oracle_text;

    // Chain of Acid's exact template (different parent effect, same copy tail).
    let parsed = parse_oracle_text(
        "Destroy target noncreature permanent. That permanent's controller may \
         copy this spell and may choose a new target for that copy.",
        "Chain of Acid",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    let ability = parsed.abilities.first().expect("has a spell ability");
    let copy = ability
        .sub_ability
        .as_ref()
        .expect("the optional copy is a sub-ability");
    assert!(
        matches!(
            *copy.effect,
            Effect::CopySpell {
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                ..
            }
        ),
        "the Chain cycle's copy clause must set MayChooseNewTargets, got {:?}",
        copy.effect
    );
}

// ---------------------------------------------------------------------------
// Runtime: cast → P1 discards → P1 (not the caster) is offered the copy →
// accept → the copy is controlled by P1 → P1 retargets to P2 → P2 discards.
// ---------------------------------------------------------------------------

#[test]
fn chain_of_smog_copy_controlled_by_targeted_player_and_retargeted() {
    let Some(db) = load_db() else {
        return;
    };

    // 3 players: P0 casts, P1 is the initial target, P2 is the retarget.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let smog = scenario.add_real_card(P0, "Chain of Smog", Zone::Hand, db);

    // Fill the targeted players' hands so the two-card discards are observable.
    for _ in 0..4 {
        scenario.add_card_to_hand(P1, "Mountain");
        scenario.add_card_to_hand(P2, "Mountain");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);

    let p1_hand_before = hand_size(&runner, P1);
    let p2_hand_before = hand_size(&runner, P2);

    let card_id = runner.state().objects[&smog].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: smog,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Chain of Smog");

    // Target P1 with the spell.
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![engine::types::ability::TargetRef::Player(P1)],
                })
                .expect("select P1 as Chain of Smog's target");
        }
        other => panic!("expected TargetSelection after casting, got {other:?}"),
    }

    // Resolve the spell off the stack. Both controllers pass priority.
    drive_to_optional_copy(&mut runner);

    // CR 707.10: "That player may copy" — the optional copy prompt goes to the
    // targeted player (P1), not the caster (P0).
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                player, P1,
                "the \"may copy\" prompt must go to the targeted player, not the caster"
            );
        }
        other => panic!("expected the optional copy prompt, got {other:?}"),
    }

    // P1 discarded two cards as the parent effect.
    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before - 2,
        "the targeted player discards two cards"
    );

    // P1 accepts the optional copy.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepts the copy");

    // CR 707.10c: the copy has a target (P1, inherited) so retarget is armed,
    // and the choosing player is the copy's controller — P1.
    let copy_id = match runner.state().waiting_for.clone() {
        WaitingFor::CopyRetarget {
            player, copy_id, ..
        } => {
            assert_eq!(
                player, P1,
                "the retarget choice belongs to the copy's controller (the targeted player)"
            );
            copy_id
        }
        other => panic!("expected CopyRetarget after accepting the copy, got {other:?}"),
    };

    // CR 707.10: the copy on the stack is controlled by P1 — not the caster.
    let copy_entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == copy_id)
        .expect("the copy is on the stack");
    assert_eq!(
        copy_entry.controller, P1,
        "CR 707.10: the spell copy is controlled by the player who put it on the stack"
    );
    assert_eq!(
        runner.state().objects[&copy_id].controller,
        P1,
        "the copy's GameObject controller must also be the targeted player"
    );

    // P1 retargets the copy to P2.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P2)),
        })
        .expect("P1 chooses P2 as the copy's new target");

    // Resolve the copy off the stack.
    drive_to_idle(&mut runner);

    // CR 707.10: resolving the copy makes the retargeted player (P2) discard
    // two cards.
    assert_eq!(
        hand_size(&runner, P2),
        p2_hand_before - 2,
        "the retargeted player (P2) discards two cards when the copy resolves"
    );
    // P1 discarded only the original two — the copy did not hit P1 again.
    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before - 2,
        "the copy was retargeted away from P1; P1 discards only the original two"
    );
}

// ---------------------------------------------------------------------------
// Runtime: the copy is itself a Chain of Smog and carries the same nested
// optional copy — accepting the re-offered copy must produce a
// second-generation copy on the stack (CR 707.10b: a copy is a new object;
// every `SelfRef` in its ability chain must resolve to the copy).
// ---------------------------------------------------------------------------

#[test]
fn chain_of_smog_nested_copy_accepted_produces_second_generation_copy() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let smog = scenario.add_real_card(P0, "Chain of Smog", Zone::Hand, db);
    // Generous hands so three two-card discards never empty a hand.
    for _ in 0..8 {
        scenario.add_card_to_hand(P1, "Mountain");
        scenario.add_card_to_hand(P2, "Mountain");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);

    let card_id = runner.state().objects[&smog].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: smog,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Chain of Smog");

    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![engine::types::ability::TargetRef::Player(P1)],
                })
                .expect("select P1 as the target");
        }
        other => panic!("expected TargetSelection, got {other:?}"),
    }

    // First copy: P1 accepts and keeps P1 as the copy's target so the copy
    // resolves against P1 again (the retarget choice is exercised by the
    // sibling test — here we want the copy to resolve and re-offer the copy).
    drive_to_optional_copy(&mut runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepts the first copy");

    let first_copy_id = match runner.state().waiting_for.clone() {
        WaitingFor::CopyRetarget { copy_id, .. } => copy_id,
        other => panic!("expected CopyRetarget for the first copy, got {other:?}"),
    };
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("P1 keeps the first copy's original target");

    // The first copy now resolves: P1 discards two more, then the *copy*
    // re-offers its own nested optional copy. Drive to that second prompt.
    drive_to_optional_copy(&mut runner);

    // CR 707.10: the second prompt belongs to the first copy's target (P1).
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(player, P1),
        other => panic!("expected the re-offered (nested) copy prompt, got {other:?}"),
    }

    // P1 accepts the re-offered copy. CR 707.10b: the first copy's nested
    // `CopySpell` must carry the FIRST copy's id as its `source_id`, so the
    // `SelfRef` resolves to the first copy and a second-generation copy is
    // created. Before the recursive `source_id` rewrite this silently failed.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepts the re-offered nested copy");

    let second_copy_id = match runner.state().waiting_for.clone() {
        WaitingFor::CopyRetarget {
            copy_id, player, ..
        } => {
            assert_eq!(player, P1, "the second copy is also controlled by P1");
            copy_id
        }
        other => panic!(
            "expected CopyRetarget for the SECOND-generation copy — the chain \
             truncated after one copy. got {other:?}"
        ),
    };

    // The second-generation copy is a distinct object on the stack.
    assert_ne!(
        second_copy_id, first_copy_id,
        "the second copy must be a new object, distinct from the first copy"
    );
    let second_copy = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == second_copy_id)
        .expect("the second-generation copy is on the stack");
    assert_eq!(
        second_copy.controller, P1,
        "CR 707.10: the second copy is controlled by the player who put it on the stack"
    );

    // Finish out: keep targets, let it resolve, decline any further copies.
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep the second copy's targets");
    drive_to_idle(&mut runner);
    assert!(
        runner.state().stack.is_empty(),
        "resolution must settle with an empty stack"
    );
}

// ---------------------------------------------------------------------------
// Runtime: declining the optional copy makes no copy and resolution completes.
// ---------------------------------------------------------------------------

#[test]
fn chain_of_smog_declined_copy_makes_no_copy() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let smog = scenario.add_real_card(P0, "Chain of Smog", Zone::Hand, db);
    for _ in 0..4 {
        scenario.add_card_to_hand(P1, "Mountain");
        scenario.add_card_to_hand(P2, "Mountain");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);

    let p1_hand_before = hand_size(&runner, P1);
    let p2_hand_before = hand_size(&runner, P2);

    let card_id = runner.state().objects[&smog].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: smog,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Chain of Smog");

    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![engine::types::ability::TargetRef::Player(P1)],
                })
                .expect("select P1 as the target");
        }
        other => panic!("expected TargetSelection, got {other:?}"),
    }

    drive_to_optional_copy(&mut runner);

    // P1 declines the copy.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P1 declines the copy");

    drive_to_idle(&mut runner);

    // The stack is empty — no copy was created.
    assert!(
        runner.state().stack.is_empty(),
        "declining the copy must leave no spell on the stack"
    );
    // Only P1 discarded; P2 untouched.
    assert_eq!(hand_size(&runner, P1), p1_hand_before - 2);
    assert_eq!(
        hand_size(&runner, P2),
        p2_hand_before,
        "declining the copy means P2 never discards"
    );
}

/// Resolve a pending `DiscardChoice` by selecting the first `count` cards.
/// Returns true if a discard choice was handled.
fn resolve_discard_choice(runner: &mut engine::game::scenario::GameRunner) -> bool {
    if let WaitingFor::DiscardChoice { count, cards, .. } = &runner.state().waiting_for {
        let chosen: Vec<ObjectId> = cards.iter().take(*count).copied().collect();
        runner
            .act(GameAction::SelectCards { cards: chosen })
            .expect("resolving the discard choice should succeed");
        return true;
    }
    false
}

/// Advance the engine (passing priority, resolving discard choices) until the
/// optional-copy prompt is surfaced.
fn drive_to_optional_copy(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::DiscardChoice { .. } => {
                resolve_discard_choice(runner);
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
    panic!("engine did not reach the optional-copy prompt");
}

/// Advance the engine until the stack is empty and both players hold priority,
/// resolving any discard choices the copy's resolution surfaces along the way.
fn drive_to_idle(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..100 {
        if resolve_discard_choice(runner) {
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            // The copy is itself a copy of Chain of Smog and carries the same
            // nested optional copy — decline it so resolution settles.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining the nested copy should succeed");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
}
