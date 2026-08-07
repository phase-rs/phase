//! Arbalest Engineers — PINNED-GREEN COMPOSITION GUARD for the #7031 modal
//! mode-body subject filter. **This test is deliberately NON-DISCRIMINATING:
//! it is green both BEFORE and AFTER the fix.**
//!
//! Why it exists anyway: Arbalest's mode 2 second sentence "It gains trample
//! and haste until end of turn" reaches the correct `ParentTarget` binding by
//! TWO different layers depending on the fix:
//!   * pre-fix: the mode body parses with the leaked `SelfRef` subject, emits
//!     `GenericEffect{affected: SelfRef}`, and the chunk-level anaphor rewrite
//!     (`replace_target_with_parent`, gated by `ctx_has_typed_trigger_subject`)
//!     repairs it post-hoc to `ParentTarget`;
//!   * post-fix: the cleared subject lets the bare-"it" branch bind
//!     `ParentTarget` at parse time, and the rewrite is a fixed point.
//!
//! Both orders converge on identical output. This guard pins that convergence
//! so a FUTURE rewrite-layer change cannot silently regress the class the
//! rewrite still covers.
//!
//! CR 608.2c: the mode's own earlier instruction ("Put a +1/+1 counter on
//! target creature") is the nearest antecedent for the mode-body "It".
//! CR 122.1: the +1/+1 counter placed by the mode's first instruction.
//!
//! Two-authority hostile fixture: the trigger source (Arbalest) and the mode's
//! target are BOTH on the battlefield; the grant must land on the target
//! (positive reach-guard) and NOT on Arbalest (negative).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const ARBALEST_ORACLE: &str = "When this creature enters, choose one —\n• This creature deals 1 damage to any target.\n• Put a +1/+1 counter on target creature. It gains trample and haste until end of turn.\n• Create a tapped Powerstone token. (It's an artifact with \"{T}: Add {C}. This mana can't be spent to cast a nonartifact spell.\")";

/// Drive the ETB trigger: choose mode 2 (index 1), target `target`, resolve.
fn drive_etb_mode_two(runner: &mut GameRunner, target: ObjectId) {
    let mut chose_mode = false;
    let mut remaining_target = Some(target);
    for _ in 0..100 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .expect("order triggers");
            }
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![1] })
                    .expect("select mode 2");
                chose_mode = true;
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let target = remaining_target
                    .take()
                    .expect("exactly one target prompt expected");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } => {
                if chose_mode && runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!(
                "unexpected WaitingFor while driving Arbalest's ETB trigger: {}",
                other.variant_name()
            ),
        }
    }
    panic!("Arbalest's ETB trigger did not resolve within the step budget");
}

#[test]
fn arbalest_mode_two_grants_trample_haste_to_target_not_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The mode's target, controlled by the opponent (two-authority fixture:
    // source and target both alive on the battlefield).
    let victim = scenario.add_creature(P1, "Pump Victim", 2, 2).id();
    // A second legal target so the engine surfaces a real target prompt.
    let _bystander = scenario.add_creature(P1, "Bystander", 1, 1).id();

    // Arbalest Engineers cast from hand (cost zeroed) so a REAL enters event
    // fires the ETB trigger through the production pipeline.
    let arbalest = scenario
        .add_creature_to_hand_from_oracle(P0, "Arbalest Engineers", 2, 2, ARBALEST_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&arbalest].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: arbalest,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Arbalest Engineers must succeed");

    drive_etb_mode_two(&mut runner, victim);

    // Positive reach-guard: the mode's first instruction resolved (CR 122.1).
    assert_eq!(
        runner.state().objects[&victim]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "mode 2 must put a +1/+1 counter on the target"
    );

    // The grant lands on the MODE'S TARGET (CR 608.2c nearest antecedent) —
    // whichever layer (parse-time binding or chunk rewrite) produced the
    // ParentTarget binding.
    let victim_obj = &runner.state().objects[&victim];
    assert!(
        victim_obj.has_keyword(&Keyword::Trample),
        "the mode's target must gain trample"
    );
    assert!(
        victim_obj.has_keyword(&Keyword::Haste),
        "the mode's target must gain haste"
    );

    // Negative (guarded by the positives above): the trigger SOURCE gets
    // neither keyword.
    let arbalest_obj = &runner.state().objects[&arbalest];
    assert!(
        !arbalest_obj.has_keyword(&Keyword::Trample),
        "the trigger source must NOT gain trample"
    );
    assert!(
        !arbalest_obj.has_keyword(&Keyword::Haste),
        "the trigger source must NOT gain haste"
    );
}
