//! Runtime pipeline regression — Knollspine Dragon (Cluster C, player-axis
//! damage-history count).
//!
//! Oracle: "Flying\nWhen this creature enters, you may discard your hand and
//! draw cards equal to the damage dealt to target opponent this turn."
//!
//! The count references the damage dealt to a TARGET opponent this turn. The
//! parser stores the record-match filter as
//! `And{[Player, Typed(controller=TargetPlayer)]}`; the fix (a) surfaces an
//! OPPONENT-scoped count-derived trigger target slot (TargetPlayer is
//! non-enumerable → would hang), and (b) lifts the controller predicate out of
//! the And in `split_controller_filter` so the player-targeted DamageRecord is
//! matched against `ability.targets` rather than failing closed.
//!
//! DISCRIMINATING: 5 damage was dealt to opponent A and 2 to opponent B earlier
//! this turn. Targeting A must draw exactly 5 — NOT 7 (the all-opponents sum,
//! which would mean the controller predicate wasn't lifted to the targeted
//! player) and NOT 0 (a hang / failed-closed match). The explicit
//! legal-actions-non-empty assertion at the trigger target step is the HANG
//! regression guard.

use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, DamageRecord, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const KNOLLSPINE_ORACLE: &str = "Flying\nWhen this creature enters, you may discard your hand and draw cards equal to the damage dealt to target opponent this turn.";

#[test]
fn knollspine_dragon_draws_target_opponents_damage_not_all_opponents() {
    let p0 = PlayerId(0);
    let p1 = PlayerId(1); // opponent A — 5 damage this turn
    let p2 = PlayerId(2); // opponent B — 2 damage this turn

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let dragon = scenario
        .add_creature_to_hand_from_oracle(p0, "Knollspine Dragon", 7, 5, KNOLLSPINE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 5,
        })
        .id();

    // P0's hand also holds two junk cards that the optional discard tosses.
    scenario.add_card_to_hand(p0, "Plains");
    scenario.add_card_to_hand(p0, "Island");

    // P0 needs at least 5 library cards to draw the full count.
    for name in ["Swamp", "Mountain", "Forest", "Plains", "Island", "Swamp"] {
        scenario.add_card_to_library_top(p0, name);
    }
    for _ in 0..3 {
        scenario.add_card_to_library_top(p1, "Plains");
        scenario.add_card_to_library_top(p2, "Plains");
    }

    // {5}{R}{R} = 7 mana.
    scenario.with_mana_pool(
        p0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, dragon, false, Vec::new()))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::Red, dragon, false, Vec::new())))
            .collect(),
    );

    let mut runner = scenario.build();

    // Seed damage history: 5 to opponent A (P1), 2 to opponent B (P2). For a
    // player target, `target_controller` is the player themselves.
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(DamageRecord {
            source_id: ObjectId(0),
            source_controller: p0,
            target: TargetRef::Player(p1),
            target_controller: p1,
            amount: 5,
            is_combat: false,
            ..Default::default()
        });
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(DamageRecord {
            source_id: ObjectId(0),
            source_controller: p0,
            target: TargetRef::Player(p2),
            target_controller: p2,
            amount: 2,
            is_combat: false,
            ..Default::default()
        });

    let card_id = runner.state().objects[&dragon].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: dragon,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Knollspine Dragon");

    // Resolve the creature onto the battlefield (pay mana, reach priority).
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty();
                break;
            }
            other => panic!("unexpected pre-ETB prompt: {other:?}"),
        }
    }

    // Snapshot P0's library size right before the ETB resolves so the draw delta
    // is measured against the library (the optional discard empties the hand
    // first, so a hand delta would be confounded).
    let library_before = runner.state().players[0].library.len();

    let mut targeted = false;
    let mut accepted_optional = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                // HANG regression: the count-derived trigger slot must enumerate
                // legal opponents. An empty slot here (TargetPlayer left
                // un-rewritten → fails closed) is a legal_actions=0 hang.
                assert!(
                    !slot.legal_targets.is_empty(),
                    "the count-derived trigger target slot must be non-empty (no hang)",
                );
                assert!(
                    slot.legal_targets.contains(&TargetRef::Player(p1))
                        && slot.legal_targets.contains(&TargetRef::Player(p2)),
                    "both opponents must be legal targets; got {:?}",
                    slot.legal_targets,
                );
                assert!(
                    !slot.legal_targets.contains(&TargetRef::Player(p0)),
                    "the controller must NOT be a legal target (opponent-scoped slot)",
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(p1)),
                    })
                    .expect("choose opponent A as the damage-history target");
                targeted = true;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional discard-and-draw");
                accepted_optional = true;
            }
            WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            other => panic!("unexpected prompt during Knollspine ETB: {other:?}"),
        }
    }

    assert!(
        targeted,
        "the count-derived trigger target slot must be selected"
    );
    assert!(
        accepted_optional,
        "the optional discard-and-draw must have prompted",
    );

    let library_after = runner.state().players[0].library.len();
    let drawn = library_before as i64 - library_after as i64;
    assert_eq!(
        drawn, 5,
        "Knollspine must draw exactly the damage dealt to the TARGETED opponent A (5) — \
         not the all-opponents sum (7) and not 0 (hang); \
         library_before={library_before}, library_after={library_after}",
    );
}
