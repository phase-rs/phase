//! Regression for issue #4247: Well Rested's granted untap trigger is
//! controlled by the *enchanted creature's* controller, not the Aura's
//! controller.
//!
//! Oracle text:
//!   Enchant creature
//!   Enchanted creature has "Whenever this creature becomes untapped, put two
//!   +1/+1 counters on it, then you gain 2 life and draw a card. This ability
//!   triggers only once each turn."
//!
//! CR 109.5 + CR 113.7/113.8 + CR 603.3a: a triggered ability's controller is
//! the player who controlled its source when it triggered. The granted ability
//! is an ability *of the enchanted creature*, so the enchanted creature's
//! controller is the "you" that gains life and draws — even when an opponent
//! controls the Aura. CR 303.4e only carves out *activated* abilities for the
//! Aura's controller; granted triggered abilities follow the host.
//!
//! So with P0's Well Rested attached to P1's creature, P1 (the host's
//! controller) gains life and draws; P0 (the Aura's controller) does not.

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::events::GameEvent;
use engine::types::phase::Phase;

const WELL_RESTED_ORACLE: &str = "Enchant creature\nEnchanted creature has \
\"Whenever this creature becomes untapped, put two +1/+1 counters on it, \
then you gain 2 life and draw a card. This ability triggers only once each turn.\"";

#[test]
fn well_rested_granted_untap_trigger_routes_to_host_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The host's controller (P1) is the "you" that draws — seed P1's library.
    scenario.with_library_top(P1, &["Forest"]);

    let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let well_rested = {
        let mut builder = scenario.add_creature(P0, "Well Rested", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura"]);
        builder.from_oracle_text(WELL_RESTED_ORACLE);
        builder.id()
    };

    let mut runner = scenario.build();

    // Attach P0's Well Rested to P1's creature.
    {
        let state = runner.state_mut();
        let aura_obj = state.objects.get_mut(&well_rested).unwrap();
        aura_obj.attached_to = Some(AttachTarget::Object(host));
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(well_rested);
    }
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), host);

    runner.state_mut().objects.get_mut(&host).unwrap().tapped = true;

    let p0_life_before = runner.life(P0);
    let p1_life_before = runner.life(P1);
    let p0_hand_before = runner.state().players[0].hand.len();
    let p1_hand_before = runner.state().players[1].hand.len();

    let events = vec![GameEvent::PermanentUntapped { object_id: host }];
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before + 2,
        "the enchanted creature's controller (P1) is the granted ability's \"you\" and gains 2 life"
    );
    assert_eq!(
        runner.state().players[1].hand.len(),
        p1_hand_before + 1,
        "the enchanted creature's controller (P1) draws from the granted trigger"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "the Aura's controller (P0) must NOT gain life from a granted triggered ability (CR 303.4e)"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        p0_hand_before,
        "the Aura's controller (P0) must NOT draw from a granted triggered ability"
    );
}
