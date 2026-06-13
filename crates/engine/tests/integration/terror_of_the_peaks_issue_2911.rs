//! Issue #2911 — Terror of the Peaks imposes an additional 3 life cost on
//! opponent spells that target it (ward-like, but not the Ward keyword).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, AdditionalCost, AdditionalCostRepeatability, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PayCostKind;

const TERROR_OF_THE_PEAKS: &str = "Flying\nSpells your opponents cast that target this creature cost an additional 3 life to cast.\nWhenever another creature you control enters, this creature deals damage equal to that creature's power to any target.";
const SACRIFICE_TARGET_SPELL: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDestroy target creature.";

fn floating_mana(generic: usize, red: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..red {
        pool.push(ManaUnit::new(
            ManaType::Red,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    pool
}

#[test]
fn terror_of_the_peaks_charges_three_life_when_targeted_by_opponent_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terror = scenario
        .add_creature_from_oracle(P0, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, floating_mana(0, 1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bolt).target_object(terror).resolve();

    outcome.assert_life_delta(P1, -3);
    outcome.assert_life_delta(P0, 0);
}

#[test]
fn terror_of_the_peaks_does_not_tax_spells_not_targeting_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _terror = scenario
        .add_creature_from_oracle(P0, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, floating_mana(0, 1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bolt).target_player(P0).resolve();

    outcome.assert_life_delta(P1, 0);
    outcome.assert_life_delta(P0, -3);
}

#[test]
fn terror_of_the_peaks_composes_with_spells_own_required_additional_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terror = scenario
        .add_creature_from_oracle(P0, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let sacrifice_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Sacrifice Target Spell", false, SACRIFICE_TARGET_SPELL)
        .id();
    let fodder = scenario.add_creature(P1, "Fodder", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let card_id = runner.state().objects[&sacrifice_spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: sacrifice_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting spell with a required additional cost should start");

    let mut sacrificed = false;
    let mut targeted = false;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(choices.contains(&fodder));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrificing the required creature should succeed");
                sacrificed = true;
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(terror)));
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(terror)],
                    })
                    .expect("targeting Terror should succeed");
                targeted = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should resolve the spell");
            }
            other => panic!("unexpected waiting state during Terror tax regression: {other:?}"),
        }
    }

    assert!(
        sacrificed,
        "the spell's own required sacrifice cost must be paid"
    );
    assert!(
        targeted,
        "the spell must target Terror for the imposed cost to apply"
    );
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P1.0 as usize].life, 17);
}

#[test]
fn terror_of_the_peaks_still_charges_when_kicker_is_declined() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terror = scenario
        .add_creature_from_oracle(P0, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let mut kicker_builder = scenario.add_creature_to_hand(P1, "Kicker Bolt", 0, 0);
    kicker_builder
        .as_instant()
        .from_oracle_text("Kicker Bolt deals 3 damage to any target.")
        .with_additional_cost(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }],
            repeatability: AdditionalCostRepeatability::Once,
        });
    let kicker_spell = kicker_builder.id();
    scenario.with_mana_pool(P1, floating_mana(0, 1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let card_id = runner.state().objects[&kicker_spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: kicker_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting kicker spell should start");

    let mut declined_kicker = false;
    let mut targeted = false;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("declining kicker should succeed");
                declined_kicker = true;
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(terror)));
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(terror)],
                    })
                    .expect("targeting Terror should succeed");
                targeted = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should resolve the spell");
            }
            other => panic!("unexpected waiting state during Terror kicker regression: {other:?}"),
        }
    }

    assert!(declined_kicker, "the kicker prompt must be exercised");
    assert!(
        targeted,
        "the spell must target Terror for the imposed cost to apply"
    );
    assert_eq!(runner.state().players[P1.0 as usize].life, 17);
}
