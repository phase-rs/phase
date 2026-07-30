//! Integration test for Triumphant Chomp (The Lost Caverns of Ixalan).
//!
//! Oracle:
//!   "Triumphant Chomp deals damage to target creature equal to 2 or the
//!    greatest power among Dinosaurs you control, whichever is greater."
//!
//! Exercises the new max-of-two-quantities combinator `QuantityExpr::Max`:
//! the damage amount is `max(2, greatest power among Dinosaurs you control)`.
//! The scenarios drive the FULL cast pipeline (parse → cast → target →
//! resolve → mark damage) and discriminate the max semantics — with either
//! the parser arm or the resolver arm reverted, the value phrase fails to
//! parse, the spell becomes Unimplemented, and every scenario marks 0 damage.
//!
//! CR 107.1 + CR 120.4a/120.10: maximum of computed integer amounts
//!   ("the greatest of the calculated amounts").
//! CR 608.2h: "the greatest power among …" is a present-tense snapshot taken
//!   as the spell resolves.
//! CR 120.3: damage dealt to a creature is marked on it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::WaitingFor;

const CHOMP_TEXT: &str = "Triumphant Chomp deals damage to target creature equal to 2 or the \
     greatest power among Dinosaurs you control, whichever is greater.";

/// Cast Triumphant Chomp targeting an opponent's 0/10 wall and return the
/// damage marked on it. `dino_power` optionally adds one Dinosaur under the
/// caster's control with that power.
fn run_chomp(dino_power: Option<i32>) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chomp_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Triumphant Chomp", false, CHOMP_TEXT)
        .id();

    // A non-Dinosaur decoy under the caster's control. It is never counted by
    // "the greatest power among Dinosaurs you control", but it guarantees ≥2
    // legal creature targets so the engine always prompts for a target
    // (a lone legal target is auto-assigned) and the test deterministically
    // aims at the opponent's wall.
    scenario.add_creature(P0, "Scout", 0, 1).id();

    if let Some(power) = dino_power {
        scenario
            .add_creature(P0, "Ranging Raptors", power, 5)
            .with_subtypes(vec!["Dinosaur"])
            .id();
    }

    // Toughness 10 so the wall survives every expected amount and the marked
    // damage can be read back exactly.
    let target_id = scenario.add_creature(P1, "Wall", 0, 10).id();

    // {R} for Triumphant Chomp.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    let chomp_card_id = runner.state().objects[&chomp_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: chomp_id,
            card_id: chomp_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Triumphant Chomp must succeed");

    let mut targeted = false;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(target_id)),
                    "the opponent's creature must be a legal target for Triumphant Chomp",
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target_id)],
                    })
                    .expect("targeting the opponent's creature must succeed");
                targeted = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state during Triumphant Chomp cast: {other:?}"),
        }
    }
    assert!(targeted, "the target-selection prompt must have fired");

    runner.state().objects[&target_id].damage_marked
}

/// max(2, 4) = 4 — the Dinosaur's power exceeds the floor, so it wins.
#[test]
fn triumphant_chomp_uses_dino_power_when_greater() {
    assert_eq!(
        run_chomp(Some(4)),
        4,
        "with a power-4 Dinosaur the amount is max(2, 4) = 4; 0 means the \
         QuantityExpr::Max parser or resolver arm was reverted",
    );
}

/// max(2, 0) = 2 — with no Dinosaurs the aggregate is 0 and the constant floor
/// wins.
#[test]
fn triumphant_chomp_uses_floor_two_without_dinosaurs() {
    assert_eq!(
        run_chomp(None),
        2,
        "with no Dinosaurs the amount is max(2, 0) = 2; 0 means the \
         QuantityExpr::Max parser or resolver arm was reverted",
    );
}

/// max(2, 1) = 2 — a small Dinosaur cannot drag the amount below the floor.
/// Distinguishes a genuine max from a "use the aggregate" mis-parse.
#[test]
fn triumphant_chomp_floor_beats_small_dino() {
    assert_eq!(run_chomp(Some(1)), 2, "max(2, 1) must be 2, not 1");
}
