//! Reproduction probe for issue #8485 (Maze of Ith still not preventing combat
//! damage in real games). The pre-existing #1094 regression only exercises the
//! SAME-controller orientation (P0 owns both the Maze and the attacker) with a
//! plain attacker. These drive the real-play orientation — the DEFENDING player
//! Mazes the ATTACKING player's creature — across the combat shapes a real game
//! actually produces (unblocked, blocked, first strike, trample, multi-block,
//! planeswalker attack, and activation after blockers are declared).

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::combat::AttackTarget;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;

const MAZE_OF_ITH: &str = "{T}: Untap target attacking creature. Prevent all combat damage that would be dealt to and dealt by that creature this turn.";

/// When the Maze is activated relative to the declare-blockers step.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MazeTiming {
    /// First P0 priority window in the declare-attackers step.
    BeforeBlockers,
    /// First P0 priority window after blockers are declared.
    AfterBlockers,
}

/// Drive P1's combat against P0, activating `maze` on `attacker` at the first
/// P0 priority window in the requested step. Runs until combat damage is done.
fn run_mazed_combat(
    runner: &mut engine::game::scenario::GameRunner,
    maze: ObjectId,
    attacker: ObjectId,
    attacks: &[(ObjectId, AttackTarget)],
    blockers: &[(ObjectId, ObjectId)],
    timing: MazeTiming,
) {
    run_mazed_combat_with_maze_removal(
        runner, maze, attacker, attacks, blockers, timing, false, false,
    )
}

/// As [`run_mazed_combat`], but optionally destroys the Maze right after its
/// ability resolves. CR 113.7a: "Once activated or triggered, an ability exists
/// on the stack independently of its source. Destruction or removal of the
/// source after that time won't affect the ability." So removing the Maze must
/// not lift the shield its resolved ability created.
#[allow(clippy::too_many_arguments)]
fn run_mazed_combat_with_maze_removal(
    runner: &mut engine::game::scenario::GameRunner,
    maze: ObjectId,
    attacker: ObjectId,
    attacks: &[(ObjectId, AttackTarget)],
    blockers: &[(ObjectId, ObjectId)],
    timing: MazeTiming,
    remove_maze_after_activation: bool,
    relayer_after_activation: bool,
) {
    let mut mazed = false;
    let mut blocked = false;
    // Reach latches for the terminal guard below. The phase check alone only proves
    // combat ENDED, which any change that pulls the attacker out of combat or skips
    // the damage step also satisfies — leaving every absence-only assertion in this
    // file green for the wrong reason, i.e. the exact class the guard exists to
    // close. These latch that the combat damage step was actually entered, and that
    // the CR 510.1c/702.19b division actually ran for the fixtures that need one.
    let mut saw_damage_step = false;
    let mut divided_damage = false;
    runner
        .declare_attackers(attacks)
        .expect("P1 must be able to declare its attack");
    for _ in 0..400 {
        saw_damage_step |= runner.state().phase == Phase::CombatDamage;
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                let b = if blocked { &[][..] } else { blockers };
                blocked = true;
                if runner.declare_blockers(b).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::Priority { player, .. }
                if player == P0 && !mazed && (timing == MazeTiming::BeforeBlockers || blocked) =>
            {
                mazed = true;
                runner.activate(maze, 0).target_object(attacker).resolve();
                // CR 701.26b reach-guard: the untap is observable, proving the
                // ability actually resolved before any damage assertion below.
                assert!(
                    !runner.state().objects[&attacker].tapped,
                    "Maze of Ith must untap the opposing attacker"
                );
                if relayer_after_activation {
                    engine::game::layers::evaluate_layers(runner.state_mut());
                }
                if remove_maze_after_activation {
                    let mut events = Vec::new();
                    engine::game::zones::move_to_zone(
                        runner.state_mut(),
                        maze,
                        engine::types::zones::Zone::Graveyard,
                        &mut events,
                    );
                }
            }
            // CR 510.1c + CR 702.19b: an attacker blocked by MORE THAN ONE creature,
            // or one with trample, must have its combat damage divided by its
            // controller before the combat damage step can proceed. Without this arm
            // the loop fell through to `_ => break` and left combat unfinished — which
            // is exactly what the terminal reach-guard below now catches, and what
            // silently hollowed out `..._prevents_trample_spillover` and
            // `..._prevents_every_event_when_multiple_blockers`.
            //
            // The division mirrors the shared driver in `rules.rs`
            // (`run_combat_with_blocker_divisions`): assign each blocker its lethal
            // minimum in order, then give the remainder to the defending player as
            // trample damage (CR 702.19b) or, with no trample, dump it on the last
            // blocker so the assignment totals the attacker's power (CR 510.1c).
            WaitingFor::AssignCombatDamage {
                blockers,
                total_damage,
                trample,
                ..
            } => {
                // NOTE: this loop matches on a CLONED `waiting_for`, so these
                // bindings are owned values, not the references `rules.rs` gets.
                let mut remaining = total_damage;
                let mut assignments: Vec<(ObjectId, u32)> = Vec::new();
                for slot in &blockers {
                    let assign = remaining.min(slot.lethal_minimum);
                    assignments.push((slot.blocker_id, assign));
                    remaining = remaining.saturating_sub(assign);
                }
                if trample.is_none() && remaining > 0 {
                    if let Some(last) = assignments.last_mut() {
                        last.1 += remaining;
                        remaining = 0;
                    }
                }
                let trample_damage = if trample.is_some() { remaining } else { 0 };
                if runner
                    .act(GameAction::AssignCombatDamage {
                        mode: engine::types::game_state::CombatDamageAssignmentMode::Normal,
                        assignments,
                        trample_damage,
                        controller_damage: 0,
                    })
                    .is_err()
                {
                    break;
                }
                divided_damage = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(mazed, "Maze of Ith must have been activated");
    // TERMINAL REACH-GUARD. Most assertions in this file are ABSENCES — "the Mazed
    // creature dealt no damage", "the planeswalker lost no loyalty", "the blocker
    // took no damage" — and an absence is satisfied for free if combat never
    // reached the combat damage step at all. This loop has six ways to stop early
    // that are not "combat finished": the `_ => break` arm on an unrecognized
    // `WaitingFor`, the four `.is_err()` breaks, and exhausting 400 iterations.
    // Without this guard a future `WaitingFor` variant, a new trigger shape, or a
    // CR 616.1 `WaitingFor::ReplacementChoice` park (which issue #8485 makes newly
    // reachable — see `prevent_damage::tests::
    // two_shields_on_one_damage_event_prevent_it_exactly_once`) would silently turn
    // eight of these tests green-for-the-wrong-reason. Only the tests that carry an
    // un-Mazed control creature are immune on their own.
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must have run to completion — an absence-only assertion below would \
         otherwise pass vacuously (stopped in {:?} waiting for {:?})",
        runner.state().phase,
        runner.state().waiting_for
    );
    // CR 510.2: reaching EndCombat is necessary but NOT sufficient. A change that
    // removed the attacker from combat, or skipped the damage step, would satisfy
    // the phase check while producing no damage event at all — and every
    // absence-only assertion below would still pass. Require that the combat damage
    // step was actually entered.
    assert!(
        saw_damage_step,
        "combat reached its end without ever entering the combat damage step — \
         no damage event was produced, so the absence assertions below prove nothing"
    );
    // CR 510.1c + CR 702.19b: for the fixtures that need a division (an attacker
    // blocked by more than one creature, or a trampler), the division must actually
    // have been performed. Without this, a future change that stopped raising
    // `AssignCombatDamage` would silently return those fixtures to the vacuous state
    // this guard was added to fix.
    //
    // Counted PER ATTACKER, not over the whole combat: CR 510.1c raises the
    // division for a single attacker blocked by two or more creatures, so a combat
    // with two attackers blocked by one creature EACH needs no division at all
    // (`issue_8485_maze_prevents_both_directions_against_opposing_attacker`). A
    // whole-combat `blockers.len() > 1` would demand a division that correctly never
    // happens and fail that fixture. `blockers` is `(blocker, attacker)` pairs.
    //
    // Looked up defensively: a panic on a missing object here would mask the
    // assertion rather than report it.
    let blockers_on_mazed = blockers.iter().filter(|(_, atk)| *atk == attacker).count();
    let needs_division = blockers_on_mazed > 1
        || runner
            .state()
            .objects
            .get(&attacker)
            .is_some_and(|o| o.has_keyword(&engine::types::keywords::Keyword::Trample));
    assert!(
        !needs_division || divided_damage,
        "the Mazed attacker is blocked by {blockers_on_mazed} creature(s) and/or has \
         trample, so a CR 510.1c damage division was required — but \
         `AssignCombatDamage` never fired"
    );
}

fn damage_marked(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner.state().objects[&obj].damage_marked
}

/// CR 615 + CR 608.2c: the defending player Mazes ONE of two UNBLOCKED opposing
/// attackers. The Mazed one deals nothing; the un-Mazed control still connects
/// for its full power — the hostile fixture proving the life assertion is not
/// vacuously satisfied by combat never happening.
#[test]
fn issue_8485_maze_prevents_only_the_mazed_unblocked_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();
    let control = scenario.add_creature(P1, "Free Attacker", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[
            (mazed, AttackTarget::Player(P0)),
            (control, AttackTarget::Player(P0)),
        ],
        &[],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "only the un-Mazed 2/2 may connect: the Mazed 3/3's combat damage to the \
         defending Maze controller must be prevented"
    );
}

/// CR 615: the Mazed attacker is BLOCKED by the Maze controller's creature.
/// Both directions are prevented; a second, un-Mazed pair in the same combat
/// takes normal damage on both sides.
#[test]
fn issue_8485_maze_prevents_both_directions_against_opposing_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 2, 3).id();
    let mazed_blocker = scenario.add_creature(P0, "Mazed Blocker", 2, 3).id();
    let free = scenario.add_creature(P1, "Free Attacker", 2, 3).id();
    let free_blocker = scenario.add_creature(P0, "Free Blocker", 2, 3).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[
            (mazed, AttackTarget::Player(P0)),
            (free, AttackTarget::Player(P0)),
        ],
        &[(mazed_blocker, mazed), (free_blocker, free)],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        damage_marked(&runner, mazed_blocker),
        0,
        "'by' shield: the Mazed attacker deals no combat damage to its blocker"
    );
    assert_eq!(
        damage_marked(&runner, mazed),
        0,
        "'to' shield: the Mazed attacker takes no combat damage"
    );
    assert_eq!(
        damage_marked(&runner, free_blocker),
        2,
        "hostile fixture: the un-Mazed pair trades normally"
    );
    assert_eq!(
        damage_marked(&runner, free),
        2,
        "hostile fixture, reverse leg"
    );
}

/// CR 615 + CR 509.1: the Maze is activated AFTER blockers are declared — the
/// normal way a defender uses it once they see the block assignment.
#[test]
fn issue_8485_maze_activated_after_blockers_still_prevents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 4, 4).id();
    let blocker = scenario.add_creature(P0, "Chump Blocker", 1, 1).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[(blocker, mazed)],
        MazeTiming::AfterBlockers,
    );

    assert_eq!(
        damage_marked(&runner, mazed),
        0,
        "'to' shield installed after blocks still prevents damage to the attacker"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        engine::types::zones::Zone::Battlefield,
        "'by' shield installed after blocks must save the 1/1 blocker"
    );
}

/// CR 615 + CR 702.19b: a Mazed TRAMPLE attacker blocked by a 1/1 must not push
/// excess damage through to the defending player — prevention applies to the
/// whole combat-damage assignment, trample spillover included.
#[test]
fn issue_8485_maze_prevents_trample_spillover() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = {
        let mut b = scenario.add_creature(P1, "Trampler", 5, 5);
        b.trample();
        b.id()
    };
    let blocker = scenario.add_creature(P0, "Chump Blocker", 1, 1).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[(blocker, mazed)],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "trample spillover from a Mazed attacker must be prevented too"
    );
    assert_eq!(
        damage_marked(&runner, blocker),
        0,
        "the blocker takes no damage from the Mazed trampler"
    );
}

/// CR 615 + CR 702.7b: a Mazed FIRST STRIKE attacker deals its damage in the
/// first-strike combat damage step (CR 510.4). The shield must apply there too.
#[test]
fn issue_8485_maze_prevents_first_strike_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = {
        let mut b = scenario.add_creature(P1, "First Striker", 3, 3);
        b.first_strike();
        b.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "first-strike combat damage from a Mazed attacker must be prevented"
    );
}

/// CR 615 + CR 508.1: an opposing creature attacking the Maze controller's
/// PLANESWALKER. The "by" shield is recipient-agnostic, so the planeswalker
/// must lose no loyalty.
#[test]
fn issue_8485_maze_prevents_damage_to_attacked_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let walker = {
        let mut b = scenario.add_creature(P0, "Loyal Walker", 0, 0);
        b.as_planeswalker_with_loyalty("Jace", 5);
        b.id()
    };
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Planeswalker(walker))],
        &[],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.state().objects[&walker].loyalty,
        Some(5),
        "a Mazed attacker must deal no combat damage to the attacked planeswalker"
    );
}

/// CR 615 + CR 509.2: the Mazed attacker is blocked by TWO creatures, so ONE
/// shield must absorb TWO damage events in the same combat damage step (two
/// "by" events out, two "to" events in). A shield that is consumed on first
/// apply would let the second blocker's damage through in each direction.
#[test]
fn issue_8485_maze_prevents_every_event_when_multiple_blockers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    // CR 510.1c: the attacker's power must be at least the SUM of both blockers'
    // lethal minimums, or the division cannot legally put damage on the second
    // blocker at all. At the original 4 power against two 4-toughness blockers the
    // only legal assignment was 4/0, so the "SECOND blocker" assertion below was
    // vacuous no matter what the shield did. 8 power forces a genuine 4/4 split, so
    // both "by" events are really produced and really have to be prevented.
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 8, 6).id();
    let blocker_a = scenario.add_creature(P0, "Blocker A", 2, 4).id();
    let blocker_b = scenario.add_creature(P0, "Blocker B", 2, 4).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[(blocker_a, mazed), (blocker_b, mazed)],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        damage_marked(&runner, blocker_a),
        0,
        "'by' shield must prevent the damage assigned to the FIRST blocker"
    );
    assert_eq!(
        damage_marked(&runner, blocker_b),
        0,
        "'by' shield must prevent the damage assigned to the SECOND blocker too"
    );
    assert_eq!(
        damage_marked(&runner, mazed),
        0,
        "'to' shield must absorb both blockers' damage, not just the first"
    );
}

/// CR 615 + CR 702.4b + CR 510.4: a Mazed DOUBLE STRIKE attacker deals combat
/// damage in TWO separate combat damage steps. The same shield must still be
/// live in the second step.
#[test]
fn issue_8485_maze_prevents_both_double_strike_steps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = {
        let mut b = scenario.add_creature(P1, "Double Striker", 3, 3);
        b.double_strike();
        b.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "BOTH the first-strike and regular combat damage steps must be prevented"
    );
}

/// CR 615 + CR 806: the same defender-side orientation in a MULTIPLAYER game —
/// the shape a real Commander table produces.
#[test]
fn issue_8485_maze_prevents_in_multiplayer() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "multiplayer: the Mazed attacker's combat damage to the Maze controller \
         must be prevented"
    );
}

/// Production parity: a real game does NOT parse Oracle text in process — it
/// loads pre-parsed `AbilityDefinition`s out of `card-data.json`. Round-trip
/// the parsed ability through serde JSON (exactly what the export/import
/// pipeline does) and re-run the prevention, so a field the export drops or
/// defaults differently cannot hide behind the in-process parse.
#[test]
fn issue_8485_maze_prevents_after_card_data_json_round_trip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();
    let control = scenario.add_creature(P1, "Free Attacker", 2, 2).id();

    let mut runner = scenario.build();

    // Serialize → deserialize the parsed abilities, then reinstall them.
    let round_tripped: Vec<engine::types::ability::AbilityDefinition> = {
        let parsed = runner.state().objects[&maze].abilities.as_ref().clone();
        let json = serde_json::to_string(&parsed).expect("abilities must serialize");
        serde_json::from_str(&json).expect("abilities must deserialize")
    };
    {
        let obj = runner.state_mut().objects.get_mut(&maze).unwrap();
        obj.abilities = std::sync::Arc::new(round_tripped.clone());
        obj.base_abilities = std::sync::Arc::new(round_tripped);
    }

    runner.state_mut().active_player = P1;
    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat(
        &mut runner,
        maze,
        mazed,
        &[
            (mazed, AttackTarget::Player(P0)),
            (control, AttackTarget::Player(P0)),
        ],
        &[],
        MazeTiming::BeforeBlockers,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "the card-data.json round-tripped ability must still prevent the Mazed \
         attacker's combat damage (only the un-Mazed 2/2 connects)"
    );
}

/// CR 113.7a: "Once activated or triggered, an ability exists on the stack
/// independently of its source. Destruction or removal of the source after that
/// time won't affect the ability." Destroying Maze of Ith after its ability has
/// resolved must NOT lift the shield — an extremely common real game line (the
/// attacker's controller answers the Maze in response to, or right after, the
/// activation).
#[test]
fn issue_8485_maze_shield_survives_the_maze_leaving_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();
    let control = scenario.add_creature(P1, "Free Attacker", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat_with_maze_removal(
        &mut runner,
        maze,
        mazed,
        &[
            (mazed, AttackTarget::Player(P0)),
            (control, AttackTarget::Player(P0)),
        ],
        &[],
        MazeTiming::BeforeBlockers,
        true,
        false,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "CR 113.7a: the prevention shield outlives Maze of Ith itself — only the \
         un-Mazed 2/2 may connect"
    );
}

/// CR 611.2c + CR 613.1: a continuous-effect (layer) re-evaluation between the
/// Maze's activation and the combat damage step must not wipe the shield it
/// installed. CR 613.1 determines an object's CHARACTERISTICS; CR 611.2c settles
/// that a prevention effect is not one of them — "An effect that reads 'Prevent
/// all damage creatures would deal this turn' doesn't modify any object's
/// characteristics, so it's modifying the rules of the game."
/// A layer pass therefore has no authority to end it. Layers are re-evaluated constantly in a real game (every ETB,
/// every cast, every trigger), so a shield that does not survive a relayer is
/// effectively never live by the time damage is dealt.
#[test]
fn issue_8485_maze_shield_survives_layer_reevaluation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();
    let control = scenario.add_creature(P1, "Free Attacker", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    let p0_life_before = runner.life(P0);
    runner.advance_to_combat();
    run_mazed_combat_with_maze_removal(
        &mut runner,
        maze,
        mazed,
        &[
            (mazed, AttackTarget::Player(P0)),
            (control, AttackTarget::Player(P0)),
        ],
        &[],
        MazeTiming::BeforeBlockers,
        false,
        true,
    );

    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "the prevention shield must survive a layer re-evaluation — only the \
         un-Mazed 2/2 may connect"
    );
}

/// CR 611.2c + CR 613.1 (issue #8485, Unit C): the RECIPIENT-hosted "dealt TO that
/// creature" half must survive a layer re-evaluation too.
///
/// The two `..._survives_...` tests above engage only the source-scoped "dealt BY"
/// half, because an UNBLOCKED Mazed attacker deals damage but is never dealt any.
/// Blocking it engages both directions: the blocker deals combat damage TO the
/// Mazed creature, which only the recipient-hosted shield prevents. That shield is
/// stored on the attacker so a zone change correctly ends it (CR 400.7), but it is
/// not one of the attacker's CHARACTERISTICS (CR 611.2c), so the CR 613.1 reseed
/// must carry it.
///
/// Revert-failing against C1: with the raw live push, the forced `evaluate_layers`
/// between the activation and the combat damage step wipes the "to" shield and the
/// blocker's damage lands on the Mazed attacker.
#[test]
fn issue_8485_maze_to_shield_survives_layer_reevaluation_when_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();
    let blocker = scenario.add_creature(P0, "Blocker", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    run_mazed_combat_with_maze_removal(
        &mut runner,
        maze,
        mazed,
        &[(mazed, AttackTarget::Player(P0))],
        &[(blocker, mazed)],
        MazeTiming::AfterBlockers,
        false,
        true,
    );

    // CR 615: the "to" half prevents the blocker's damage to the Mazed creature.
    assert_eq!(
        damage_marked(&runner, mazed),
        0,
        "CR 611.2c + CR 613.1: a layer pass must not wipe the recipient-hosted \\
         \"dealt to that creature\" shield"
    );
    // CR 615: and the "by" half still prevents the Mazed creature's damage, so the
    // blocker survives — the reach-guard that the block really happened.
    assert_eq!(
        damage_marked(&runner, blocker),
        0,
        "the source-scoped \"dealt by that creature\" shield still applies"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        engine::types::zones::Zone::Battlefield,
        "the blocker must survive"
    );
}

/// CR 611.2c + CR 613.1: STORAGE-level pin for the same defect the behavioral
/// tests above cover. Maze of Ith installs two shields — a SOURCE-scoped "dealt
/// by that creature" half (CR 113.7a: it must not ride on the Maze, so it lives
/// in `state.pending_damage_replacements`) and a RECIPIENT-hosted "dealt to that
/// creature" half (hosted on the attacker, because CR 400.7 says it should die
/// when that creature changes zones). Neither is an object CHARACTERISTIC
/// (CR 611.2c), so neither may be destroyed by a CR 613.1 layer pass.
///
/// This test asserts the counts directly rather than reporting them, so a
/// regression that silently drops a store is caught here and not only through a
/// life-total assertion three combat steps later.
#[test]
fn issue_8485_shield_storage_survives_layer_reevaluation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maze = scenario
        .add_land_from_oracle(P0, "Maze of Ith", MAZE_OF_ITH)
        .id();
    let mazed = scenario.add_creature(P1, "Mazed Attacker", 3, 3).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(mazed, AttackTarget::Player(P0))])
        .expect("attack");
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player == P0)
        {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner.activate(maze, 0).target_object(mazed).resolve();
    // CR 701.26b reach-guard: the untap proves the ability actually resolved, so
    // the shield counts below are not vacuously zero-vs-zero.
    assert!(
        !runner.state().objects[&mazed].tapped,
        "Maze of Ith must untap the opposing attacker"
    );

    let registry_before = runner.state().pending_damage_replacements.len();
    let host_live_before = runner.state().objects[&mazed].replacement_definitions.len();
    assert_eq!(
        registry_before, 1,
        "CR 113.7a: the source-scoped \"dealt by that creature\" half must live in \
         the floating registry, not on the Maze"
    );
    assert_eq!(
        host_live_before, 1,
        "CR 400.7: the recipient-hosted \"dealt to that creature\" half must live on \
         the attacker it is scoped to"
    );

    engine::game::layers::evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        registry_before,
        "CR 611.2c + CR 613.1: a layer pass must not touch the floating registry"
    );
    assert_eq!(
        runner.state().objects[&mazed].replacement_definitions.len(),
        host_live_before,
        "CR 611.2c + CR 613.1: the CR 613.1 reseed must CARRY the recipient-hosted \
         resolution shield, not wipe it — a prevention effect is not an object \
         characteristic"
    );
}
