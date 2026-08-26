//! Regression: The Sixth Doctor's Demonstrate spell copy must not retain the
//! source card's commander designation and block state-based actions after a
//! Localized Destruction board wipe.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LOCALIZED_DESTRUCTION: &str = "You get {E} (an energy counter), then you may pay one or more {E}. If you do, each creature you control with power equal to the amount of {E} paid this way gains indestructible until end of turn.\nDestroy all creatures.";

/// CR 903.3 + CR 707.10 + CR 704.5d: The Sixth Doctor grants Demonstrate to
/// commander Sarah Jane Smith. Its spell copies become tokens only after they
/// resolve, so Localized Destruction must offer the real card's command-zone
/// return and then remove the copied token without a second unreachable choice.
#[test]
fn localized_destruction_does_not_deadlock_on_a_copied_commander_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "The Sixth Doctor",
        2,
        4,
        "The first nonartifact spell you cast each turn has demonstrate.",
    );
    let sarah = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarah Jane Smith", 3, 4, "")
        .commander()
        .id();
    let localized_destruction = scenario
        .add_spell_to_hand_from_oracle(P0, "Localized Destruction", false, LOCALIZED_DESTRUCTION)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    // CR 702.144a: accept Demonstrate's self-copy. The opponent copy is also
    // produced by the keyword, but all copies must shed the source's commander
    // designation before resolving.
    runner.cast(sarah).accept_optional().resolve();

    let copied_sarah_ids: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.name == "Sarah Jane Smith" && obj.is_token)
        .map(|obj| obj.id)
        .collect();
    assert!(
        !copied_sarah_ids.is_empty(),
        "CR 702.144a: Demonstrate must create at least its self-copy"
    );
    assert!(
        copied_sarah_ids.iter().all(|id| {
            let copy = &runner.state().objects[id];
            !copy.is_commander && !copy.is_signature_spell()
        }),
        "CR 903.3: copied spells are tokens, not command-zone cards"
    );
    assert!(
        runner.state().objects[&sarah].is_commander,
        "the original Sarah Jane Smith card remains the commander"
    );

    // Resolve the actual first line of Localized Destruction explicitly. Its
    // energy-payment choice must settle before the following board wipe runs.
    let mut destruction = runner.cast(localized_destruction).commit();
    destruction
        .act(GameAction::PassPriority)
        .expect("first priority pass accepts the cast");
    let energy_optional = destruction
        .act(GameAction::PassPriority)
        .expect("second priority pass begins Localized Destruction resolution");
    assert!(matches!(
        energy_optional.waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    destruction
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining Localized Destruction's energy payment is valid");
    drop(destruction);

    assert_eq!(
        runner.state().players[P0.0 as usize].energy,
        1,
        "declining the optional payment leaves the energy granted by the spell unspent"
    );

    // The decline completes the spell at a priority boundary. Finish that
    // priority round so the engine performs the next SBA check, whose
    // commander-return choice is the behavior under regression.
    for _ in 0..runner.state().players.len() {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance the post-resolution SBA pipeline");
    }

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone: Zone::Graveyard,
            ..
        } if commander_id == sarah
    ));

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the genuine commander return choice must be actionable");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "after the genuine choice, priority must progress instead of surfacing a token choice"
    );
    assert_eq!(runner.state().objects[&sarah].zone, Zone::Command);
    assert!(
        copied_sarah_ids
            .iter()
            .all(|id| !runner.state().objects.contains_key(id)),
        "CR 704.5d: copied Sarah Jane tokens must cease to exist after the wipe"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ),
        "no second CommanderZoneChoice may strand the player"
    );
}
