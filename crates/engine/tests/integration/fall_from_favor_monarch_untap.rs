//! Fall from Favor: the untap restriction names the enchanted creature's
//! current controller, and CR 725.5 makes it inactive while no one is monarch.

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ContinuousModification, Duration, PlayerScope, StaticCondition, TargetFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

const FALL_FROM_FAVOR: &str = "Enchant creature\nWhen this Aura enters, tap enchanted creature and you become the monarch.\nEnchanted creature doesn't untap during its controller's untap step unless that player is the monarch.";
const P2: PlayerId = PlayerId(2);

fn attach_aura(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    attach_to(runner.state_mut(), aura, host);
    assert_eq!(
        runner.state().objects[&aura]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(host),
        "the printed Aura must legally attach to its recipient",
    );
}

fn board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    let enchanted = scenario.add_creature(P1, "Enchanted Bear", 2, 2).id();
    let free = scenario.add_creature(P1, "Free Bear", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Fall from Favor", FALL_FROM_FAVOR)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    attach_aura(&mut runner, aura, enchanted);
    for id in [enchanted, free] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    (runner, aura, enchanted, free)
}

fn assert_parsed_gate(runner: &GameRunner, aura: ObjectId) {
    let gate = runner.state().objects[&aura]
        .static_definitions
        .iter_unchecked()
        .find(|def| def.mode == StaticMode::CantUntap)
        .expect("the printed CantUntap static must be present");
    assert_eq!(
        gate.condition,
        Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::IsMonarch {
                player: PlayerScope::RecipientController,
            }),
        })
    );
}

#[test]
fn enchanted_creature_untaps_only_when_its_controller_is_monarch_or_monarch_is_vacant() {
    for (monarch, stays_tapped) in [(Some(P0), true), (None, false), (Some(P1), false)] {
        let (mut runner, aura, enchanted, free) = board();
        assert_parsed_gate(&runner, aura);
        runner.state_mut().monarch = monarch;
        runner.state_mut().layers_dirty.mark_full();
        runner.advance_to_phase(Phase::Upkeep);
        assert_eq!(
            runner.state().active_player,
            P1,
            "phase={:?} waiting={:?}",
            runner.state().phase,
            runner.state().waiting_for
        );
        assert_eq!(
            runner.state().objects[&enchanted].tapped,
            stays_tapped,
            "monarch={monarch:?}"
        );
        assert!(
            !runner.state().objects[&free].tapped,
            "the same untap step must free an unenchanted creature"
        );
    }
}

#[test]
fn two_auras_bind_their_own_recipients() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Plains", "Plains", "Plains"]);
    }
    let p1_creature = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let p2_creature = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let p1_free = scenario.add_creature(P1, "P1 Free Bear", 2, 2).id();
    let p2_free = scenario.add_creature(P2, "P2 Free Bear", 2, 2).id();
    let aura1 = scenario
        .add_enchantment_from_oracle(P0, "Fall from Favor A", FALL_FROM_FAVOR)
        .with_subtypes(vec!["Aura"])
        .id();
    let aura2 = scenario
        .add_enchantment_from_oracle(P0, "Fall from Favor B", FALL_FROM_FAVOR)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    attach_aura(&mut runner, aura1, p1_creature);
    attach_aura(&mut runner, aura2, p2_creature);
    assert_parsed_gate(&runner, aura1);
    assert_parsed_gate(&runner, aura2);
    for id in [p1_creature, p2_creature, p1_free, p2_free] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner.state_mut().monarch = Some(P1);
    runner.state_mut().layers_dirty.mark_full();
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "phase={:?} waiting={:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert!(!runner.state().objects[&p1_creature].tapped);
    assert!(!runner.state().objects[&p1_free].tapped);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("P1 can pass combat");
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P2,
        "phase={:?} waiting={:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert!(runner.state().objects[&p2_creature].tapped);
    assert!(!runner.state().objects[&p2_free].tapped);
}

#[test]
fn control_change_rebinds_the_untap_subject() {
    for (monarch, stays_tapped) in [(P1, false), (P0, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for player in [P0, P1] {
            scenario.with_library_top(player, &["Plains", "Plains", "Plains"]);
        }
        let enchanted = scenario.add_creature(P0, "Enchanted Bear", 2, 2).id();
        let free = scenario.add_creature(P1, "P1 Free Bear", 2, 2).id();
        let fall = scenario
            .add_enchantment_from_oracle(P0, "Fall from Favor", FALL_FROM_FAVOR)
            .with_subtypes(vec!["Aura"])
            .id();
        let mut runner = scenario.build();
        attach_aura(&mut runner, fall, enchanted);
        assert_parsed_gate(&runner, fall);
        // CR 613.1b: install the same layer-2 transient control effect used by
        // gain_control::resolve, with a P1-controlled source.
        runner.state_mut().add_transient_continuous_effect(
            free,
            P1,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: enchanted },
            vec![ContinuousModification::ChangeController],
            None,
        );
        // Layer 2 changes the creature's live controller, while Fall from Favor
        // remains controlled by P0 and the creature's base controller stays P0.
        runner.state_mut().layers_dirty.mark_full();
        evaluate_layers(runner.state_mut());
        assert_eq!(runner.state().objects[&enchanted].base_controller, Some(P0));
        assert_eq!(runner.state().objects[&enchanted].controller, P1);
        assert_eq!(runner.state().objects[&fall].controller, P0);

        for id in [enchanted, free] {
            runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
        }
        runner.state_mut().monarch = Some(monarch);
        runner.state_mut().layers_dirty.mark_full();
        runner.advance_to_phase(Phase::Upkeep);
        assert_eq!(runner.state().phase, Phase::Upkeep);
        assert_eq!(runner.state().active_player, P1);
        assert_eq!(runner.state().objects[&enchanted].controller, P1);
        assert_eq!(runner.state().objects[&fall].controller, P0);
        assert_eq!(
            runner.state().objects[&enchanted].tapped,
            stays_tapped,
            "monarch={monarch:?}"
        );
        assert!(
            !runner.state().objects[&free].tapped,
            "P1's free creature must untap"
        );
    }
}
