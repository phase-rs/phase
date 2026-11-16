//! Runtime coverage for "Whenever a creature dies, untap this creature"
//! (Goblin Sharpshooter), reported twice: issue #8116 (the Sharpshooter kills a
//! creature with its own ping and stays tapped) and issue #6389 (it doesn't
//! untap when someone's commander dies).
//!
//! CR 700.4: "dies" means "is put into a graveyard from the battlefield", so the
//! trigger watches a battlefield-to-graveyard zone change for any creature, not
//! just the controller's own. CR 704.5g destroys a creature whose marked damage
//! is at least its toughness, and CR 603.2 then triggers the untap ability.
//!
//! These tests drive the REAL activation pipeline
//! (`GameAction::ActivateAbility` through `apply()` via the
//! `GameScenario`/`GameRunner` harness) with the card's verbatim Oracle text, so
//! they exercise the live parser and the production resolution path rather than
//! a hand-built AST.
//!
//! WHY THE POSITIVE ASSERTION IS NOT VACUOUS: "the Sharpshooter is untapped"
//! would also hold if the `{T}` cost had never tapped it in the first place.
//! `sharpshooter_stays_tapped_when_no_creature_dies` is the paired reach-guard:
//! the same activation against a creature that SURVIVES the ping leaves the
//! Sharpshooter tapped, which proves the cost really does tap it and that the
//! untap in the first test is a state change caused by the dies trigger.
//! Each test also asserts the victim's final zone, so a test can't pass by
//! failing to deal damage at all.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Verbatim Oracle text (Scryfall, oracle id `d81285b7-a718-411a-8be3-ecc0cfe0bcb0`).
const GOBLIN_SHARPSHOOTER: &str = "This creature doesn't untap during your untap step.\n\
                                   Whenever a creature dies, untap this creature.\n\
                                   {T}: This creature deals 1 damage to any target.";

/// Resolve the `{T}` ping by its cost rather than hardcoding an index, so an
/// added ability can't silently retarget the test at the wrong one.
fn tap_ping_index(runner: &GameRunner, id: ObjectId) -> usize {
    let object = &runner.state().objects[&id];
    let candidates: Vec<usize> = object
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| matches!(ability.cost, Some(AbilityCost::Tap)))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "reach guard: Goblin Sharpshooter must parse to exactly one {{T}} activated ability, \
         got abilities {:?}",
        object
            .abilities
            .iter()
            .map(|ability| ability.description.clone())
            .collect::<Vec<_>>()
    );
    candidates[0]
}

/// Build the board: P0's Sharpshooter plus one P1 creature of the given size.
fn board(victim_power: i32, victim_toughness: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sharpshooter = scenario
        .add_creature_from_oracle(P0, "Goblin Sharpshooter", 1, 1, GOBLIN_SHARPSHOOTER)
        .id();
    let victim = scenario
        .add_creature(P1, "Victim", victim_power, victim_toughness)
        .id();
    let runner = scenario.build();
    (runner, sharpshooter, victim)
}

/// Issue #8116: the Sharpshooter's own ping kills a creature, so its
/// "whenever a creature dies" trigger must untap it (CR 700.4 + CR 704.5g).
#[test]
fn sharpshooter_untaps_when_a_creature_dies() {
    let (mut runner, sharpshooter, victim) = board(1, 1);

    assert!(
        !runner.state().objects[&sharpshooter].tapped,
        "precondition: the Sharpshooter starts untapped"
    );

    let index = tap_ping_index(&runner, sharpshooter);
    let outcome = runner
        .activate(sharpshooter, index)
        .target_object(victim)
        .resolve();
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // CR 704.5g: 1 damage is lethal to a 1/1, so the victim is destroyed and
    // CR 700.4 "dies" is satisfied. Without this the untap assertion below
    // would be testing nothing.
    assert_eq!(
        runner.state().objects.get(&victim).map(|o| o.zone),
        Some(Zone::Graveyard),
        "reach guard: the ping must actually kill the victim (CR 704.5g); outcome zone was {:?}",
        outcome.zone_of(victim)
    );

    // CR 603.2: the dies trigger fires and untaps the Sharpshooter.
    assert!(
        !runner.state().objects[&sharpshooter].tapped,
        "CR 700.4 + CR 603.2: 'Whenever a creature dies, untap this creature' must untap the \
         Sharpshooter after its own ping kills a creature"
    );
}

/// Paired reach-guard and negative sibling: when nothing dies, the `{T}` cost
/// leaves the Sharpshooter tapped. This is what proves the untap asserted above
/// is a real state change rather than a permanent that was never tapped.
#[test]
fn sharpshooter_stays_tapped_when_no_creature_dies() {
    let (mut runner, sharpshooter, victim) = board(2, 2);

    let index = tap_ping_index(&runner, sharpshooter);
    let outcome = runner
        .activate(sharpshooter, index)
        .target_object(victim)
        .resolve();
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // CR 704.5g: 1 damage is not lethal to a 2/2, so nothing dies.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "reach guard: a 2/2 must survive one damage"
    );
    assert_eq!(
        outcome.damage_marked(victim),
        1,
        "reach guard: the ping must still have resolved and marked its damage"
    );

    // CR 701.26b: nothing untapped it, so the paid `{T}` cost stands.
    assert!(
        runner.state().objects[&sharpshooter].tapped,
        "the {{T}} activation cost must leave the Sharpshooter tapped when no creature dies"
    );
}

/// Issue #8116 as actually reported: the kill comes from DEATHTOUCH (Basilisk
/// Collar grants it to the equipped creature), which is CR 704.5h — a different
/// state-based action from the lethal-damage one above (CR 704.5g). The victim
/// survives on raw damage, so only the deathtouch SBA can destroy it; if dies
/// triggers were skipped on that branch, the two tests above would still pass.
#[test]
fn sharpshooter_untaps_when_its_deathtouch_ping_kills() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sharpshooter = scenario
        .add_creature_from_oracle(P0, "Goblin Sharpshooter", 1, 1, GOBLIN_SHARPSHOOTER)
        // CR 702.2b: Basilisk Collar grants deathtouch to the equipped creature;
        // granting the keyword directly keeps the test on the SBA under
        // examination rather than on equipment attachment.
        .with_keyword(Keyword::Deathtouch)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 3, 3).id();
    let mut runner = scenario.build();

    let index = tap_ping_index(&runner, sharpshooter);
    runner
        .activate(sharpshooter, index)
        .target_object(victim)
        .resolve();
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // CR 704.5h: one deathtouch damage destroys a 3/3 even though CR 704.5g
    // would not, so reaching the graveyard also proves the ping resolved —
    // nothing else on this board can kill a 3/3.
    assert_eq!(
        runner.state().objects.get(&victim).map(|o| o.zone),
        Some(Zone::Graveyard),
        "reach guard: CR 704.5h must destroy the 3/3 via deathtouch, not CR 704.5g"
    );

    assert!(
        !runner.state().objects[&sharpshooter].tapped,
        "CR 704.5h + CR 700.4: a deathtouch kill is still a death, so the dies trigger must \
         untap the Sharpshooter"
    );
}

/// Issue #6389: a dying COMMANDER. CR 903.9a is a state-based action, not a
/// replacement effect — the commander really is put into the graveyard from the
/// battlefield first, so CR 700.4 "dies" is satisfied and dies triggers fire.
/// Only CR 903.9b (hand/library) replaces the move. If the engine instead
/// treated the command-zone return as a replacement, the trigger's
/// Battlefield -> Graveyard zone change would never match.
#[test]
fn sharpshooter_untaps_when_a_commander_dies() {
    // CR 903.9a only applies in a format with a command zone; `sba.rs`'s
    // `check_commander_zone_return` returns early unless
    // `format_config.command_zone` is set. A default-format scenario leaves the
    // dead commander in the graveyard and never offers the return, which would
    // make this test a duplicate of the plain lethal-damage case above.
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let sharpshooter = scenario
        .add_creature_from_oracle(P0, "Goblin Sharpshooter", 1, 1, GOBLIN_SHARPSHOOTER)
        .id();
    let commander = scenario
        .add_creature(P1, "Opposing Commander", 1, 1)
        .commander()
        .id();
    let mut runner = scenario.build();

    let index = tap_ping_index(&runner, sharpshooter);
    let outcome = runner
        .activate(sharpshooter, index)
        .target_object(commander)
        .resolve();

    // Reach guard for the whole point of this test: the CR 903.9a state-based
    // action must actually be offered. Without this, a format or eligibility
    // regression would silently downgrade this test into the plain
    // lethal-damage case and it would still pass.
    let WaitingFor::CommanderZoneChoice {
        player,
        commander_id,
        current_zone,
    } = outcome.final_waiting_for()
    else {
        panic!(
            "reach guard: CR 903.9a must offer the command-zone return after the commander dies; \
             halted in {:?} instead",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(*commander_id, commander);
    assert_eq!(*current_zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Graveyard,
        "CR 903.9a + CR 700.4: the commander is in the GRAVEYARD when the choice is offered — \
         it really died, which is what makes the dies trigger legal"
    );

    // Accept the return (the harder case for the reported bug: the commander
    // does not stay in the graveyard), then let the parked trigger batch run.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the commander's owner may accept the CR 903.9a return");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Command,
        "the accepted CR 903.9a return moves the commander on to the command zone"
    );
    assert!(
        !runner.state().objects[&sharpshooter].tapped,
        "CR 903.9a + CR 700.4: the command-zone return is a state-based action AFTER the \
         commander reaches the graveyard, so it dies and the dies trigger must untap the \
         Sharpshooter"
    );
}
