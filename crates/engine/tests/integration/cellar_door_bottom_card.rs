//! Cellar Door (issue #8989 F2): "{3}, {T}: Target player puts the bottom card
//! of their library into their graveyard. If it's a creature card, you create a
//! 2/2 black Zombie creature token."
//!
//! Before the fix the "Target player" subject target was not transferred to the
//! Mill effect — "their library" resolved to TriggeringPlayer, so the mill
//! targeted the ability's controller instead of the announced player target.

use engine::game::effects::stack_reach::stack_entry_node_reach;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text (Scryfall).
const CELLAR_DOOR: &str = "{3}, {T}: Target player puts the bottom card of their library into their graveyard. If it's a creature card, you create a 2/2 black Zombie creature token.";

/// Verbatim printed Oracle text (Scryfall): a mill replacement that names
/// opponents of its controller.
const BRUVAC: &str = "If an opponent would mill one or more cards, they mill twice that many cards instead. (To mill a card, a player puts the top card of their library into their graveyard.)";

/// Verbatim printed Oracle text (Scryfall): a per-card graveyard redirect.
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

/// Verbatim printed Oracle text (Scryfall): a genuine (top-edge) mill.
const THOUGHT_SCOUR: &str = "Target player mills two cards.\nDraw a card.";

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
    stage_cellar_door_with(bottom_is_creature, target_player, |_| {})
}

/// `stage_cellar_door`, with `extra` adding permanents to the board first.
fn stage_cellar_door_with(
    bottom_is_creature: bool,
    target_player: TargetPlayer,
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId, PlayerId) {
    let mut scenario = GameScenario::new();
    extra(&mut scenario);
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

/// Activate Cellar Door, choose `target_player`, and pay, stopping with the
/// ability on the stack and its controller holding priority.
fn announce_cellar_door(
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
    events
}

fn activate_cellar_door(
    runner: &mut GameRunner,
    cellar_door: ObjectId,
    target_player: PlayerId,
) -> Vec<GameEvent> {
    let mut events = announce_cellar_door(runner, cellar_door, target_player);

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

/// The `acted_on` stack reach of the first `Mill` node on the stack entry whose
/// source is `source`.
fn mill_node_reach(runner: &GameRunner, source: ObjectId) -> Vec<TargetRef> {
    let state = runner.state();
    let entry = state
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .expect("the ability must be on the stack");
    stack_entry_node_reach(state, entry)
        .into_iter()
        .find(|reach| matches!(reach.node.effect, Effect::Mill { .. }))
        .expect("the entry must hold a Mill node")
        .acted_on
}

fn add_bruvac(scenario: &mut GameScenario) {
    scenario.add_creature_from_oracle(P0, "Bruvac the Grandiloquent", 1, 4, BRUVAC);
}

/// CR 701.17a + CR 614.1a: putting the bottom card of a library into a
/// graveyard is not a mill, so Bruvac's "If an opponent would mill" never
/// applies to Cellar Door, and stack reach keeps the chosen opponent.
#[test]
fn cellar_door_stack_reach_keeps_the_chosen_player_under_bruvac() {
    let (mut runner, cellar_door, bottom, target) = stage_cellar_door_with(true, 1, add_bruvac);
    announce_cellar_door(&mut runner, cellar_door, target);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must stop with the ability on the stack"
    );
    assert_eq!(
        mill_node_reach(&runner, cellar_door),
        vec![TargetRef::Player(P1)],
        "Bruvac replaces mills only; the bottom-card move acts on the chosen player"
    );

    // The live resolver agrees: exactly the bottom card moves, not two cards.
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&bottom].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        1,
        "Bruvac must not double a bottom-card move"
    );
}

/// Control for the test above: a genuine top-edge mill of Bruvac's opponent
/// still meets Bruvac's replacement, so stack reach answers nothing for it.
#[test]
fn thought_scour_stack_reach_defers_to_bruvac_for_a_genuine_mill() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_bruvac(&mut scenario);
    for i in 0..4 {
        scenario.add_card_to_library_top(P1, &format!("{FILLER} {i}"));
    }
    let scour = scenario
        .add_spell_to_hand_from_oracle(P0, "Thought Scour", true, THOUGHT_SCOUR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&scour].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: scour,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Thought Scour must be castable");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select the opponent");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast must stop with the spell on the stack"
    );
    assert!(
        mill_node_reach(&runner, scour).is_empty(),
        "a genuine mill under Bruvac is replaced, so stack reach cannot name its player"
    );

    // Reach guard: Bruvac really applies to this mill (two cards become four).
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].graveyard.len(), 4);
}

/// CR 614.6: the bottom card's own move still meets per-card `Moved`
/// redirects; with Rest in Peace on the board stack reach answers nothing for
/// Cellar Door's move, as for any replaced move.
#[test]
fn cellar_door_stack_reach_defers_to_a_per_card_graveyard_redirect() {
    let (mut runner, cellar_door, bottom, target) = stage_cellar_door_with(true, 1, |scenario| {
        scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    });
    announce_cellar_door(&mut runner, cellar_door, target);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must stop with the ability on the stack"
    );
    assert!(
        mill_node_reach(&runner, cellar_door).is_empty(),
        "Rest in Peace replaces the bottom card's move, so stack reach cannot name it"
    );

    // Reach guard: Rest in Peace really redirects the bottom card.
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&bottom].zone, Zone::Exile);
}

/// Verbatim printed Oracle text (Scryfall): a card carrying its own
/// from-anywhere graveyard replacement.
const BLIGHTSTEEL_COLOSSUS: &str = "Trample, infect, indestructible\nIf Blightsteel Colossus would be put into a graveyard from anywhere, reveal Blightsteel Colossus and shuffle it into its owner's library instead.";

/// Cellar Door on the stack targeting P1, whose library holds a Grizzly Bears
/// and a Blightsteel Colossus under two fillers. `colossus_on_bottom` picks
/// which of the two hidden cards is the bottommost; nothing visible differs.
fn stage_hidden_bottom_pair(colossus_on_bottom: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let cellar_door = scenario
        .add_artifact_from_oracle(P0, "Cellar Door", CELLAR_DOOR)
        .id();
    // `add_*_to_library_top` seats each card at the top (CR 401.2), so the
    // card added first ends up bottommost.
    let add_bears = |scenario: &mut GameScenario| {
        scenario
            .add_spell_to_library_top(P1, "Grizzly Bears", false)
            .as_creature();
    };
    let add_colossus = |scenario: &mut GameScenario| {
        scenario
            .add_spell_to_library_top(P1, "Blightsteel Colossus", false)
            .as_creature()
            .from_oracle_text(BLIGHTSTEEL_COLOSSUS);
    };
    if colossus_on_bottom {
        add_colossus(&mut scenario);
        add_bears(&mut scenario);
    } else {
        add_bears(&mut scenario);
        add_colossus(&mut scenario);
    }
    for i in 0..2 {
        scenario.add_card_to_library_top(P1, &format!("{FILLER} {i}"));
    }
    let mut runner = scenario.build();
    announce_cellar_door(&mut runner, cellar_door, P1);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must stop with the ability on the stack"
    );
    (runner, cellar_door)
}

/// CR 401.2 + CR 400.2: the library is a face-down pile in a hidden zone, so
/// stack reach must not tell which card is on its bottom. Swapping the two
/// hidden cards (Grizzly Bears, Blightsteel Colossus) leaves the answer for
/// Cellar Door's move unchanged; the Bruvac and Rest in Peace tests above
/// remain the controls for a public replacement.
#[test]
fn cellar_door_stack_reach_is_invariant_to_the_hidden_bottom_card() {
    let (bears_bottom, bears_door) = stage_hidden_bottom_pair(false);
    let (colossus_bottom, colossus_door) = stage_hidden_bottom_pair(true);

    // Reach guard: the two boards really differ only in the hidden bottom card.
    let bottom_name = |runner: &GameRunner| {
        let state = runner.state();
        let bottom = *state.players[1].library.last().expect("staged library");
        state.objects[&bottom].name.clone()
    };
    assert_eq!(bottom_name(&bears_bottom), "Grizzly Bears");
    assert_eq!(bottom_name(&colossus_bottom), "Blightsteel Colossus");

    let reach = mill_node_reach(&bears_bottom, bears_door);
    assert_eq!(
        reach,
        vec![TargetRef::Player(P1)],
        "no public replacement applies, so the move acts on the chosen player"
    );
    assert_eq!(
        mill_node_reach(&colossus_bottom, colossus_door),
        reach,
        "which hidden card is on the bottom must not change stack reach"
    );
}
