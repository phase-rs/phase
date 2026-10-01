use engine::ai_support::legal_actions;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, TargetFilter};
use engine::types::actions::GameAction;

const LEONIN_BOLA: &str =
    "Equipped creature has \"{T}, Unattach Leonin Bola: Tap target creature.\"\nEquip {1}";

#[test]
fn leonin_bola_granted_ability_on_creature_with_activated_ability() {
    let mut scenario = GameScenario::new();

    // Create target creature for opponent
    let target = scenario.add_creature(P1, "Bear", 2, 2);
    let target_id = target.id();

    // Create host creature with an intrinsic activated ability ({T}: Deal 1 damage to any target)
    let host = scenario.add_creature_from_oracle(
        P0,
        "Prodigal Pyromancer",
        1,
        1,
        "{T}: ~ deals 1 damage to any target.",
    );
    let host_id = host.id();

    // Create Leonin Bola Equipment
    let mut bola = scenario.add_artifact_from_oracle(P0, "Leonin Bola", LEONIN_BOLA);
    bola.with_subtypes(vec!["Equipment"]);
    let bola_id = bola.id();

    let mut runner = scenario.build();

    // Attach Leonin Bola to host
    {
        let bola_obj = runner.state_mut().objects.get_mut(&bola_id).unwrap();
        bola_obj.attached_to = Some(AttachTarget::Object(host_id));
        let host_obj = runner.state_mut().objects.get_mut(&host_id).unwrap();
        host_obj.attachments.push(bola_id);
    }

    // Reapply layers so the static ability on Leonin Bola grants the ability to host
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());

    // Host should now have 2 abilities:
    // 0: {T}: ~ deals 1 damage to any target.
    // 1: {T}, Unattach Leonin Bola: Tap target creature.
    let host_obj = runner.state().objects.get(&host_id).unwrap();
    assert_eq!(host_obj.abilities.len(), 2);
    let can_activate =
        engine::game::casting::can_activate_ability_now(runner.state(), P0, host_id, 1);
    assert!(
        can_activate,
        "Leonin Bola's granted ability should be activatable"
    );

    // Get legal actions
    let legal = legal_actions(runner.state());
    let host_activations: Vec<_> = legal
        .iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == host_id => Some(*ability_index),
            _ => None,
        })
        .collect();

    // Both ability 0 and ability 1 should be legally activatable
    assert!(
        host_activations.contains(&0),
        "host's intrinsic ability 0 should be activatable: {host_activations:?}"
    );
    assert!(
        host_activations.contains(&1),
        "Leonin Bola's granted ability 1 should be activatable: {host_activations:?}"
    );

    // Target creature should initially be untapped
    assert!(!runner.state().objects.get(&target_id).unwrap().tapped);

    // Activate Leonin Bola's granted ability (index 1) targeting opponent's creature
    runner
        .activate(host_id, 1)
        .target_object(target_id)
        .resolve();

    // After activation, cost is paid: host is tapped, Leonin Bola is unattached
    assert!(
        runner.state().objects.get(&host_id).unwrap().tapped,
        "host should be tapped as cost"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&bola_id)
            .unwrap()
            .attached_to
            .is_none(),
        "Leonin Bola should be unattached as cost"
    );
    assert!(
        !runner
            .state()
            .objects
            .get(&host_id)
            .unwrap()
            .attachments
            .contains(&bola_id),
        "host attachments should no longer contain Leonin Bola"
    );

    // Ability resolved: target creature is now tapped!
    assert!(
        runner.state().objects.get(&target_id).unwrap().tapped,
        "target creature should be tapped by resolved ability"
    );
}

#[test]
fn leonin_bola_granted_ability_distinct_controllers() {
    // CR 301.5d + CR 701.3d: P0 controls Leonin Bola, attached to P1's creature.
    // P1 activates the granted ability and unattaches P0's Leonin Bola.
    let mut scenario = GameScenario::new();

    // Target creature for P0
    let target = scenario.add_creature(P0, "Bear", 2, 2);
    let target_id = target.id();

    // Host creature controlled by P1
    let host = scenario.add_creature_from_oracle(
        P1,
        "Prodigal Pyromancer",
        1,
        1,
        "{T}: ~ deals 1 damage to any target.",
    );
    let host_id = host.id();

    // Leonin Bola controlled by P0
    let mut bola = scenario.add_artifact_from_oracle(P0, "Leonin Bola", LEONIN_BOLA);
    bola.with_subtypes(vec!["Equipment"]);
    let bola_id = bola.id();

    let mut runner = scenario.build();

    // Attach P0's Leonin Bola to P1's host
    {
        let bola_obj = runner.state_mut().objects.get_mut(&bola_id).unwrap();
        assert_eq!(bola_obj.controller, P0);
        bola_obj.attached_to = Some(AttachTarget::Object(host_id));
        let host_obj = runner.state_mut().objects.get_mut(&host_id).unwrap();
        assert_eq!(host_obj.controller, P1);
        host_obj.attachments.push(bola_id);
    }

    // Flush layers so P1's host receives the granted ability
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());

    // P1's host should have ability 1 activatable by P1
    let can_activate =
        engine::game::casting::can_activate_ability_now(runner.state(), P1, host_id, 1);
    assert!(
        can_activate,
        "P1 should be able to activate Leonin Bola's granted ability on P1's creature"
    );

    // P1 activates granted ability targeting P0's creature (set priority to P1)
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    runner
        .activate(host_id, 1)
        .target_object(target_id)
        .resolve();

    // After activation, P1's host is tapped, P0's Leonin Bola is unattached but still controlled by P0
    assert!(
        runner.state().objects.get(&host_id).unwrap().tapped,
        "P1's host should be tapped as cost"
    );
    let bola_obj = runner.state().objects.get(&bola_id).unwrap();
    assert!(
        bola_obj.attached_to.is_none(),
        "P0's Leonin Bola should be unattached as cost"
    );
    assert_eq!(
        bola_obj.controller, P0,
        "P0 should still control Leonin Bola after unattachment"
    );

    // Target creature is tapped
    assert!(
        runner.state().objects.get(&target_id).unwrap().tapped,
        "target creature should be tapped by resolved ability"
    );
}

#[test]
fn leonin_bola_granted_ability_distinct_granters() {
    // Two Bolas grant abilities to the same host. Activate the second granter
    // so attachment-to-host eligibility alone cannot select the right Equipment.
    let mut scenario = GameScenario::new();

    let target = scenario.add_creature(P1, "Bear", 2, 2);
    let target_id = target.id();

    let host1 = scenario.add_creature(P0, "Creature 1", 1, 1);
    let host1_id = host1.id();

    let mut bola1 = scenario.add_artifact_from_oracle(P0, "Leonin Bola", LEONIN_BOLA);
    bola1.with_subtypes(vec!["Equipment"]);
    let bola1_id = bola1.id();

    let mut bola2 = scenario.add_artifact_from_oracle(P0, "Leonin Bola", LEONIN_BOLA);
    bola2.with_subtypes(vec!["Equipment"]);
    let bola2_id = bola2.id();

    let mut runner = scenario.build();

    // Attach both Bolas to Host 1
    {
        let b1 = runner.state_mut().objects.get_mut(&bola1_id).unwrap();
        b1.attached_to = Some(AttachTarget::Object(host1_id));
        runner
            .state_mut()
            .objects
            .get_mut(&host1_id)
            .unwrap()
            .attachments
            .push(bola1_id);

        let b2 = runner.state_mut().objects.get_mut(&bola2_id).unwrap();
        b2.attached_to = Some(AttachTarget::Object(host1_id));
        runner
            .state_mut()
            .objects
            .get_mut(&host1_id)
            .unwrap()
            .attachments
            .push(bola2_id);
    }

    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());

    let abilities = &runner.state().objects[&host1_id].abilities;
    assert_eq!(abilities.len(), 2, "both Bolas must grant an ability");
    let ability_index = abilities
        .iter()
        .position(|ability| match ability.cost.as_ref() {
            Some(AbilityCost::Composite { costs }) => costs.iter().any(|cost| {
                matches!(
                    cost,
                    AbilityCost::Unattach {
                        target: Some(TargetFilter::SpecificObject { id })
                    } if *id == bola2_id
                )
            }),
            _ => false,
        })
        .expect("Bola 2's granted cost must bind Bola 2");
    assert!(!runner.state().objects[&target_id].tapped);

    runner
        .activate(host1_id, ability_index)
        .target_object(target_id)
        .resolve();

    assert!(
        runner.state().objects[&bola2_id].attached_to.is_none(),
        "Bola 2 should be unattached"
    );
    assert_eq!(
        runner.state().objects[&bola1_id].attached_to,
        Some(AttachTarget::Object(host1_id)),
        "Bola 1 should remain attached to the same host"
    );
    assert!(runner.state().objects[&target_id].tapped);
}
