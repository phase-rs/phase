//! Issue #1092 — Chthonian Nightmare's activated ability could never be paid.
//!
//! Oracle text:
//!   "When this enchantment enters, you get {E}{E}{E} (three energy counters).
//!   Pay X {E}, Sacrifice a creature, Return this enchantment to its owner's
//!   hand: Return target creature card with mana value X from your graveyard
//!   to the battlefield. Activate only as a sorcery."
//!
//! Root cause: `try_parse_energy_cost` (crates/engine/src/parser/oracle_cost.rs)
//! had no variable-X branch, so "Pay X {E}" silently parsed as `PayEnergy {
//! amount: Fixed(0) }` instead of `PayEnergy { amount: Ref(Variable("X")) }`
//! (mirroring the existing "pay x life" / "pay x speed" branches). With X
//! hard-pinned to 0: no energy was ever charged, no X-announcement step ever
//! fired, and the ability's own effect ("mana value X") only ever matched a
//! mana-value-0 graveyard creature — so on any realistic board (nonzero-MV
//! creatures in the graveyard, the entire point of the card) the ability had
//! no legal target and was, in the reporter's words, never activatable.
//!
//! This test drives the real `parse_oracle_text` + activation pipeline end to
//! end: activate, announce X = 2, sacrifice a creature, and reanimate the
//! chosen mana-value-2 graveyard creature — while a mana-value-0 decoy in the
//! same graveyard proves X is actually threaded into the target filter (pre-fix,
//! X was pinned to 0 and only the decoy would ever be a legal target).

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHTHONIAN_NIGHTMARE_ORACLE: &str = "When this enchantment enters, you get {E}{E}{E} (three energy counters).\nPay X {E}, Sacrifice a creature, Return this enchantment to its owner's hand: Return target creature card with mana value X from your graveyard to the battlefield. Activate only as a sorcery.";

#[test]
fn chthonian_nightmare_activates_pays_x_energy_and_reanimates_by_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![]),
        ],
    );

    let nightmare = scenario
        .add_creature(P0, "Chthonian Nightmare", 0, 0)
        .as_enchantment()
        .from_oracle_text(CHTHONIAN_NIGHTMARE_ORACLE)
        .id();
    let sacrifice = scenario.add_creature(P0, "Sacrifice Me", 1, 1).id();

    // Mana-value-0 decoy: pre-fix, X was pinned to 0, so this would be the
    // ONLY legal target — the actual bug's fingerprint.
    let _decoy_mv0 = scenario
        .add_creature_to_graveyard(P0, "Decoy MV0", 1, 1)
        .id();
    // Two mana-value-2 creatures, only matched when X is genuinely announced.
    // A second same-MV candidate (mirroring `issue_1021_recurring_nightmare`'s
    // approach) keeps target selection genuinely interactive instead of the
    // engine silently auto-selecting a sole legal target.
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Return", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();
    let _other_gy_creature = scenario
        .add_creature_to_graveyard(P0, "Other Graveyard Resident", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].energy = 3;

    let ability_index = runner.state().objects[&nightmare]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, engine::types::ability::AbilityKind::Activated))
        .expect("activated ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: nightmare,
            ability_index,
        })
        .expect("begin activation");

    let mut saw_choose_x = false;
    let mut saw_sacrifice = false;
    let mut saw_target = false;

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { min, max, .. } => {
                assert_eq!(min, 0, "X may legally be 0");
                assert_eq!(
                    max, 3,
                    "max X must be bounded by the player's 3 energy counters (pre-fix: no announcement ever fired)"
                );
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("announce X = 2");
                saw_choose_x = true;
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![sacrifice],
                    })
                    .expect("sacrifice creature");
                saw_sacrifice = true;
            }
            WaitingFor::TargetSelection { ref selection, .. } => {
                assert!(
                    selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(gy_creature)),
                    "mana-value-2 creature must be a legal target when X = 2"
                );
                assert!(
                    !selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(_decoy_mv0)),
                    "mana-value-0 decoy must NOT be a legal target when X = 2 \
                     (pre-fix, X was pinned to 0 and only this decoy would ever qualify)"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(gy_creature)],
                    })
                    .expect("select the mana-value-2 graveyard creature");
                saw_target = true;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(gy_creature)],
                    })
                    .expect("select the mana-value-2 graveyard creature");
                saw_target = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay activation mana from pool");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve ability");
            }
            other => panic!("unexpected waiting state during activation: {other:?}"),
        }
    }

    assert!(saw_choose_x, "activation must require announcing X");
    assert!(
        saw_sacrifice,
        "activation must require sacrificing a creature"
    );
    assert!(
        saw_target,
        "activation must require choosing a graveyard creature"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].energy,
        1,
        "3 energy - X(2) = 1 must actually be deducted"
    );
    assert_eq!(
        runner.state().objects[&nightmare].zone,
        Zone::Hand,
        "Chthonian Nightmare must return itself to hand as part of the activation cost"
    );
    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Battlefield,
        "ability must reanimate the chosen mana-value-2 graveyard creature"
    );
    assert_eq!(
        runner.state().objects[&sacrifice].zone,
        Zone::Graveyard,
        "sacrificed creature must be in the graveyard"
    );
}
