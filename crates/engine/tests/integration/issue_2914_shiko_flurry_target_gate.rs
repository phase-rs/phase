//! Issue #2914: Shiko and Narset, Unified — Flurry must copy the second spell
//! only when it targets a permanent or player; otherwise the reflexive draw fires.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;
use engine::types::Phase;

const SHIKO: &str = "Flying, vigilance\nFlurry — Whenever you cast your second spell each turn, copy that spell if it targets a permanent or player, and you may choose new targets for the copy. If you don't copy a spell this way, draw a card.";

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

fn opt_drawn(runner: &GameRunner) -> bool {
    runner
        .state()
        .objects
        .values()
        .any(|o| o.name == "Opt" && o.zone == Zone::Hand && o.owner == P0)
}

/// CR 608.2c + CR 707.10: a targeted second spell is copied, so Flurry must not draw.
#[test]
fn targeted_second_spell_is_copied_not_drawn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Shiko and Narset, Unified", 3, 3, SHIKO);
    let bolt1 = scenario.add_bolt_to_hand(P0);
    let bolt2 = scenario.add_bolt_to_hand(P0);
    scenario.add_spell_to_library_top(P0, "Opt", true);
    scenario.with_mana_pool(P0, red_mana(6));
    let mut runner = scenario.build();

    cast(&mut runner, bolt1);
    cast(&mut runner, bolt2);

    assert!(
        !opt_drawn(&runner),
        "Flurry copied the targeted second spell, so it must not draw."
    );
    assert_eq!(
        runner.life(P1),
        11,
        "two Lightning Bolts plus the copied second Bolt should deal 9 total damage"
    );
}

/// CR 608.2c + CR 707.10: an untargeted second spell is not copied, so Flurry draws.
#[test]
fn untargeted_second_spell_draws_instead_of_copying() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Shiko and Narset, Unified", 3, 3, SHIKO);
    let bolt = scenario.add_bolt_to_hand(P0);
    let draw_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Simple Draw", true, "Draw a card.")
        .id();
    // Flurry resolves before the draw spell and takes Opt; Simple Draw needs a card below it.
    scenario.add_spell_to_library_top(P0, "Filler", true);
    scenario.add_spell_to_library_top(P0, "Opt", true);
    scenario.with_mana_pool(P0, red_mana(3));
    let mut runner = scenario.build();

    cast(&mut runner, bolt);
    cast(&mut runner, draw_spell);

    assert!(
        opt_drawn(&runner),
        "Flurry did not copy the untargeted second spell, so it must draw."
    );
}
