//! "Deals damage equal to the power of the chosen/revealed (beheld) object."
//!
//! Close Encounter and Monstrous Emergence both impose an additional cost to
//! "choose a creature you control or reveal a creature card from your hand"
//! (a `BeholdCostAction::ChooseOrReveal` cost), then deal damage to target
//! creature equal to the POWER of that chosen/revealed object — NOT the target's
//! own power, and NOT zero.
//!
//! These tests drive the real cast pipeline: cast the spell, pay the behold cost
//! by selecting a power-5 creature, target an opponent creature with toughness
//! greater than 5, resolve, and assert exactly 5 damage is marked. The chosen
//! creature is stamped as the spell's `cost_paid_object` (CR 400.7j), and the
//! amount resolves through `QuantityRef::Power { scope: CostPaidObject }`
//! (CR 208.1 + CR 608.2 — power read at resolution).
//!
//! Revert-proof: if the amount referent were dropped (resolving to 0) or
//! mis-bound to the target's own power, the marked-damage assertion fails. The
//! 6-toughness target and 5-power chosen creature are distinct so the wrong
//! reading (target power) and the right reading (chosen power) cannot coincide.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const MONSTROUS_EMERGENCE_ORACLE: &str = "As an additional cost to cast this spell, choose a \
creature you control or reveal a creature card from your hand.\nMonstrous Emergence deals damage \
equal to the power of the creature you chose or the card you revealed to target creature.";

const CLOSE_ENCOUNTER_CHOOSE_ONLY_ORACLE: &str =
    "As an additional cost to cast this spell, choose \
a creature you control or reveal a creature card from your hand.\nClose Encounter deals damage \
equal to the power of the chosen creature or card to target creature.";

/// Drive every additional-cost / targeting prompt the cast surfaces, choosing
/// `behold_choice` for the behold cost and `target` for the damage target.
/// Stops once the spell has resolved off the stack.
fn pay_behold_and_resolve(runner: &mut GameRunner, behold_choice: ObjectId, target: ObjectId) {
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                assert!(
                    choices.contains(&behold_choice),
                    "the power-5 creature must be an eligible behold choice: {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![behold_choice],
                    })
                    .expect("paying the behold cost by choosing the creature must succeed");
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(target)),
                    "the opponent creature must be a legal damage target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target)],
                    })
                    .expect("targeting the opponent creature must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!("unexpected prompt while casting behold-damage spell: {other:?}"),
        }
    }
    panic!("cast pipeline did not settle within the prompt budget");
}

fn cast_behold_damage_spell(oracle: &str, spell_name: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The chosen creature: power 5, low toughness. Its POWER is the damage amount.
    let chosen = scenario.add_creature(P0, "Power Five Beast", 5, 1).id();
    // The opponent's creature: toughness 6 (> 5) so it survives 5 damage but the
    // exact marked-damage value still discriminates the amount referent. Its own
    // power (2) is deliberately different from the chosen creature's power (5).
    let target = scenario.add_creature(P1, "Tough Wall", 2, 6).id();

    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, spell_name, true, oracle);
    builder.with_mana_cost(engine::types::mana::ManaCost::generic(0));
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the behold-damage spell must be accepted");

    pay_behold_and_resolve(&mut runner, chosen, target);
    (runner, target)
}

/// Monstrous Emergence: damage equals the chosen creature's power (5), dealt to
/// the targeted opponent creature.
#[test]
fn monstrous_emergence_deals_chosen_power_to_target() {
    let (runner, target) =
        cast_behold_damage_spell(MONSTROUS_EMERGENCE_ORACLE, "Monstrous Emergence");

    assert_eq!(
        runner.state().objects[&target].damage_marked,
        5,
        "Monstrous Emergence must deal damage equal to the CHOSEN creature's power (5), \
         not 0 (dropped referent) and not the target's own power (2)"
    );
}

/// Close Encounter's choose-a-creature leg: damage equals the chosen creature's
/// power (5). (The exile/"warped" leg is honestly deferred — see report.)
#[test]
fn close_encounter_choose_leg_deals_chosen_power_to_target() {
    let (runner, target) =
        cast_behold_damage_spell(CLOSE_ENCOUNTER_CHOOSE_ONLY_ORACLE, "Close Encounter");

    assert_eq!(
        runner.state().objects[&target].damage_marked,
        5,
        "Close Encounter (choose leg) must deal damage equal to the CHOSEN creature's power (5)"
    );
}
