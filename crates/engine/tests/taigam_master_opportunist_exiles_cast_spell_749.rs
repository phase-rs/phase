//! Issue #749: Taigam, Master Opportunist must exile the *cast spell* (with four
//! time counters), not itself.
//!
//! Flurry — "Whenever you cast your second spell each turn, copy it, then exile
//! the spell you cast with four time counters on it. If it doesn't have suspend,
//! it gains suspend."
//!
//! Pre-fix, "exile the spell you cast" parsed to `ChangeZone { target:
//! ParentTarget }`. On a SpellCast trigger ParentTarget finds no event-context
//! match and falls through to `use_self`, exiling Taigam (the trigger source).
//! The "four time counters" clause was also dropped. This test drives the real
//! cast pipeline and discriminates all three gaps:
//!   (a) Taigam stays on the battlefield (regression direction — pre-fix it is
//!       exiled);
//!   (b) the cast spell is the object exiled;
//!   (c) the exiled spell carries 4 time counters (CR 702.62).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;
use engine::types::Phase;

const TAIGAM: &str = "Flurry — Whenever you cast your second spell each turn, copy it, then exile the spell you cast with four time counters on it. If it doesn't have suspend, it gains suspend.";

fn red_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        })
        .collect()
}

fn card_id_of(runner: &GameRunner, id: ObjectId) -> CardId {
    runner.state().objects.get(&id).unwrap().card_id
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = card_id_of(runner, spell);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast spell");
    drive(runner);
}

/// Drive the pipeline to stack-empty: player targets choose P1, trigger/copy
/// targets keep their default, priority passes.
fn drive(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let t = slot
                    .legal_targets
                    .iter()
                    .find(|t| matches!(t, TargetRef::Player(P1)))
                    .or_else(|| slot.legal_targets.first())
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose cast target");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let t = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose trigger target");
            }
            WaitingFor::CopyRetarget {
                target_slots,
                current_slot,
                ..
            } => {
                let keep = target_slots[current_slot].current.clone();
                runner
                    .act(GameAction::ChooseTarget { target: keep })
                    .expect("keep copy target");
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
}

/// Issue #749 regression: casting the controller's second spell fires Flurry,
/// which must exile the *cast spell* (not Taigam) with four time counters.
#[test]
fn taigam_exiles_cast_spell_not_itself_with_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let taigam = scenario
        .add_creature_from_oracle(P0, "Taigam, Master Opportunist", 3, 4, TAIGAM)
        .id();
    let bolt1 = scenario.add_bolt_to_hand(P0);
    let bolt2 = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(P0, red_mana(6));
    let mut runner = scenario.build();

    cast(&mut runner, bolt1);
    cast(&mut runner, bolt2);

    // (a) Regression direction: pre-fix Taigam itself is exiled by the
    // misparsed `ParentTarget → use_self` fallthrough. It must remain on the
    // battlefield.
    assert!(
        runner.state().battlefield.contains(&taigam),
        "CR 608.2k: Taigam must stay on the battlefield, not exile itself. \
         Taigam zone = {:?}",
        runner.state().objects.get(&taigam).map(|o| o.zone),
    );

    // (b) The cast second spell (bolt2) is the object that was exiled.
    let bolt2_obj = runner
        .state()
        .objects
        .get(&bolt2)
        .expect("the cast spell object must still exist");
    assert_eq!(
        bolt2_obj.zone,
        Zone::Exile,
        "CR 608.2k: the spell you cast (bolt2) must be exiled, got {:?}",
        bolt2_obj.zone,
    );
    assert!(
        runner.state().exile.contains(&bolt2),
        "the cast spell must be in the exile zone",
    );

    // (c) The exiled cast spell carries four time counters (CR 702.62), parsed
    // from "four" — not Taigam, and not zero.
    let time_counters = bolt2_obj
        .counters
        .get(&CounterType::Time)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        time_counters, 4,
        "CR 702.62: the exiled spell must carry four time counters, got {time_counters}",
    );
}
