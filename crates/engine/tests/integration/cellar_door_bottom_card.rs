//! Cellar Door (issue #8989 F2): "{3}, {T}: Target player puts the bottom card
//! of their library into their graveyard. If it's a creature card, you create a
//! 2/2 black Zombie creature token."
//!
//! Before the fix the "Target player" subject target was not transferred to the
//! Mill effect — "their library" resolved to TriggeringPlayer, so the mill
//! targeted the ability's controller instead of the announced player target.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text (Scryfall).
const CELLAR_DOOR: &str = "{3}, {T}: Target player puts the bottom card of their library into their graveyard. If it's a creature card, you create a 2/2 black Zombie creature token.";

/// A basic creature to use as the bottom card.
const BOTTOM_CREATURE: &str = "Bottom Creature";
const BOTTOM_CREATURE_POWER: i32 = 2;
const BOTTOM_CREATURE_TOUGHNESS: i32 = 2;

/// A noncreature to use as the bottom card.
const BOTTOM_NONCREATURE: &str = "Bottom Noncreature";

/// A filler card for the library top.
const FILLER: &str = "Filler";

/// A 2/2 Zombie token (what Cellar Door creates).
const ZOMBIE_TOKEN: &str = "Zombie";

/// Target player: 0 = P0 (controller), 1 = P1 (opponent)
type TargetPlayer = usize;

struct CellarDoorRun {
    runner: GameRunner,
    bottom: ObjectId,
    events: Vec<GameEvent>,
}

fn stage_cellar_door(
    bottom_is_creature: bool,
    target_player: TargetPlayer,
) -> (GameRunner, ObjectId, ObjectId, PlayerId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    // Add Cellar Door to P0's battlefield
    let cellar_door = scenario
        .add_artifact_from_oracle(P0, "Cellar Door", CELLAR_DOOR)
        .id();

    // Add a creature for P0 and P1 so they have something on board
    scenario.add_creature_from_oracle(P0, "P0 Creature", 2, 2, "Grizzly Bears");
    scenario.add_creature_from_oracle(P1, "P1 Creature", 2, 2, "Grizzly Bears");

    // Add bottom card to target player's library
    let target = if target_player == 0 { P0 } else { P1 };
    let bottom = if bottom_is_creature {
        scenario
            .add_spell_to_library_top(target, BOTTOM_CREATURE, true)
            .as_creature()
            .id()
    } else {
        scenario.add_card_to_library_top(target, BOTTOM_NONCREATURE)
    };

    // Add filler cards on top
    for i in 0..3 {
        scenario.add_card_to_library_top(target, &format!("{FILLER} {i}"));
    }

    let mut runner = scenario.build();

    // Set the bottom creature's stats if it's a creature
    if bottom_is_creature {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&bottom)
            .expect("staged bottom card");
        obj.power = Some(BOTTOM_CREATURE_POWER);
        obj.toughness = Some(BOTTOM_CREATURE_TOUGHNESS);
        obj.base_power = Some(BOTTOM_CREATURE_POWER);
        obj.base_toughness = Some(BOTTOM_CREATURE_TOUGHNESS);
    }

    (runner, cellar_door, bottom, target)
}

fn activate_cellar_door(
    runner: &mut GameRunner,
    cellar_door: ObjectId,
    target_player: PlayerId,
) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: cellar_door,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Cellar Door's ability must be activatable")
        .events;

    // The ability has a target player - select the target
    for _ in 0..8 {
        let waiting_for = runner.state().waiting_for.clone();
        match waiting_for {
            WaitingFor::TargetSelection { target_slots, .. } => {
                // Find the player target slot and select the target player
                let mut found = false;
                for slot in &target_slots {
                    // Look for a player target slot
                    for legal_target in &slot.legal_targets {
                        if let TargetRef::Player(pid) = legal_target {
                            if *pid == target_player {
                                events.extend(
                                    runner
                                        .act(GameAction::SelectTargets {
                                            targets: vec![TargetRef::Player(target_player)],
                                        })
                                        .expect("select target player")
                                        .events,
                                );
                                found = true;
                                break;
                            }
                        }
                    }
                    if found {
                        break;
                    }
                }
                if !found {
                    panic!(
                        "Could not find target player {:?} in target slots",
                        target_player
                    );
                }
            }
            WaitingFor::Priority { .. } => break,
            WaitingFor::ManaPayment { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pay {3} from the pool")
                        .events,
                );
            }
            other => panic!(
                "Cellar Door's activation must reach target selection or priority, got {other:?}"
            ),
        }
    }

    // Resolve the ability
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority")
                        .events,
                );
            }
            WaitingFor::ManaPayment { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pay mana")
                        .events,
                );
            }
            other => panic!(
                "Cellar Door's activation must resolve without interactive choice, got {other:?}"
            ),
        }
    }

    events
}

fn run_cellar_door(bottom_is_creature: bool, target_player: TargetPlayer) -> CellarDoorRun {
    let (mut runner, cellar_door, bottom, target) =
        stage_cellar_door(bottom_is_creature, target_player);
    let events = activate_cellar_door(&mut runner, cellar_door, target);
    CellarDoorRun {
        runner,
        bottom,
        events,
    }
}

#[test]
fn cellar_door_targets_opponent_bottom_creature_goes_to_opponent_graveyard() {
    // Target P1 (opponent), bottom card is a creature
    let run = run_cellar_door(true, 1);
    let target_player = P1;

    // The bottom card should go to P1's graveyard (the target player)
    assert_eq!(
        run.runner.state().objects[&run.bottom].zone,
        Zone::Graveyard,
        "the bottom creature must go to the TARGET player's graveyard"
    );
    assert_eq!(
        run.runner.state().objects[&run.bottom].controller,
        target_player,
        "the bottom creature's controller must be the target player"
    );

    // P1's graveyard should have the bottom card
    assert_eq!(
        run.runner.state().players[1].graveyard.len(),
        1,
        "P1's graveyard must have the bottom card"
    );
    assert_eq!(
        run.runner.state().players[1].graveyard[0],
        run.bottom,
        "the bottom card must be in P1's graveyard"
    );

    // P0's graveyard should be empty
    assert!(
        run.runner.state().players[0].graveyard.is_empty(),
        "P0's graveyard must be empty"
    );

    // The token should be created for P0 (the controller of Cellar Door)
    let zombie = run
        .runner
        .state()
        .objects
        .values()
        .find(|o| o.name == ZOMBIE_TOKEN && o.controller == P0)
        .expect("Zombie token must be created for Cellar Door's controller");
    assert_eq!(
        zombie.zone,
        Zone::Battlefield,
        "Zombie token must be on battlefield"
    );
    assert_eq!(zombie.power, Some(2));
    assert_eq!(zombie.toughness, Some(2));

    // The top filler cards should stay in P1's library
    let library_count = run
        .runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Library && o.controller == target_player)
        .count();
    assert_eq!(
        library_count, 3,
        "the 3 filler cards must remain in the target player's library"
    );

    assert!(
        matches!(run.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority"
    );

    // CR 701.17a: putting the bottom card into a graveyard is not milling, so no
    // mill trigger may observe it.
    assert!(
        !run.events
            .iter()
            .any(|event| matches!(event, GameEvent::Milled { .. })),
        "a bottom-edge move must not publish Milled"
    );
}

#[test]
fn cellar_door_targets_opponent_bottom_noncreature_stays_in_opponent_graveyard() {
    // Target P1 (opponent), bottom card is a noncreature
    let run = run_cellar_door(false, 1);
    let target_player = P1;

    // The bottom card should go to P1's graveyard (the target player)
    assert_eq!(
        run.runner.state().objects[&run.bottom].zone,
        Zone::Graveyard,
        "the bottom noncreature must go to the TARGET player's graveyard"
    );
    assert_eq!(
        run.runner.state().objects[&run.bottom].controller,
        target_player,
        "the bottom noncreature's controller must be the target player"
    );

    // P1's graveyard should have the bottom card
    assert_eq!(
        run.runner.state().players[1].graveyard.len(),
        1,
        "P1's graveyard must have the bottom card"
    );

    // P0's graveyard should be empty
    assert!(
        run.runner.state().players[0].graveyard.is_empty(),
        "P0's graveyard must be empty"
    );

    // No Zombie token should be created
    let zombie = run
        .runner
        .state()
        .objects
        .values()
        .find(|o| o.name == ZOMBIE_TOKEN && o.controller == P0);
    assert!(
        zombie.is_none(),
        "no Zombie token must be created for noncreature"
    );

    // The top filler cards should stay in P1's library
    let library_count = run
        .runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Library && o.controller == target_player)
        .count();
    assert_eq!(
        library_count, 3,
        "the 3 filler cards must remain in the target player's library"
    );

    assert!(
        matches!(run.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority"
    );
}

#[test]
fn cellar_door_targets_self_bottom_creature_goes_to_own_graveyard() {
    // Target P0 (self), bottom card is a creature
    let run = run_cellar_door(true, 0);
    let target_player = P0;

    // The bottom card should go to P0's graveyard (the target player = self)
    assert_eq!(
        run.runner.state().objects[&run.bottom].zone,
        Zone::Graveyard,
        "the bottom creature must go to the TARGET player's graveyard"
    );
    assert_eq!(
        run.runner.state().objects[&run.bottom].controller,
        target_player,
        "the bottom creature's controller must be the target player"
    );

    // P0's graveyard should have the bottom card
    assert_eq!(
        run.runner.state().players[0].graveyard.len(),
        1,
        "P0's graveyard must have the bottom card"
    );

    // The token should be created for P0
    let zombie = run
        .runner
        .state()
        .objects
        .values()
        .find(|o| o.name == ZOMBIE_TOKEN && o.controller == P0)
        .expect("Zombie token must be created for Cellar Door's controller");
    assert_eq!(
        zombie.zone,
        Zone::Battlefield,
        "Zombie token must be on battlefield"
    );

    // The top filler cards should stay in P0's library
    let library_count = run
        .runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Library && o.controller == target_player)
        .count();
    assert_eq!(
        library_count, 3,
        "the 3 filler cards must remain in the target player's library"
    );

    assert!(
        matches!(run.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority"
    );
}
