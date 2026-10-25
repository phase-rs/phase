//! Mass effects restricted by a combat-history relative clause, and the
//! self-blocker "it" antecedent (issue #9179 parts B and C).
//!
//! "each creature it blocked this combat" (Cathedral Membrane) and "all
//! creatures it blocked this turn" (Defiant Vanguard) must reach only the
//! creatures the source blocked in that window (CR 509.1g + CR 608.2i), and the
//! window must not be read as an effect duration (CR 611.2a). A self-subject
//! block trigger's bare "it" (Cinder Wall, Wall of Junk, Ceremonial Guard,
//! Zephyr Spirit) names the blocker itself, not the creature it blocked
//! (CR 608.2k).
//!
//! Every card is built from its verbatim Oracle text and driven through real
//! `DeclareAttackers` / `DeclareBlockers` / `PassPriority` actions to an empty
//! stack in the postcombat main phase.

use engine::game::combat::{AttackTarget, BlockHistoryPair};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CATHEDRAL_MEMBRANE: &str = "({W/P} can be paid with either {W} or 2 life.)
Defender
When this creature dies during combat, it deals 6 damage to each creature it blocked this combat.";
const DEFIANT_VANGUARD: &str = "When this creature blocks, at end of combat, destroy it and all creatures it blocked this turn.
{5}, {T}: Search your library for a Rebel permanent card with mana value 4 or less, put it onto the battlefield, then shuffle.";
const CINDER_WALL: &str = "Defender
When this creature blocks, destroy it at end of combat.";
const WALL_OF_JUNK: &str = "Defender (This creature can't attack.)
When this creature blocks, return it to its owner's hand at end of combat. (Return it only if it's on the battlefield.)";
const CEREMONIAL_GUARD: &str = "When this creature attacks or blocks, destroy it at end of combat.";
const ZEPHYR_SPIRIT: &str = "When this creature blocks, return it to its owner's hand.";
const FULL_THROTTLE: &str = "After this main phase, there are two additional combat phases.
At the beginning of each combat this turn, untap all creatures that attacked this turn.";

/// Drive from `PreCombatMain` through one declare-blockers step: pass to
/// declare attackers, declare `attacks`, pass the post-attack priority window
/// (resolving any attack trigger on the stack), then declare `blocks`. Leaves
/// the runner right after blockers are declared.
fn drive_declare_blockers(
    runner: &mut GameRunner,
    attacks: Vec<(ObjectId, AttackTarget)>,
    blocks: Vec<(ObjectId, ObjectId)>,
) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    for _ in 0..20 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("PassPriority should succeed");
    }
    runner
        .act(GameAction::DeclareBlockers {
            assignments: blocks,
        })
        .expect("DeclareBlockers should succeed");
}

/// The action that advances the game one step without making any combat
/// declarations; panics on a prompt these fixtures never expect.
fn advance_action(runner: &GameRunner) -> GameAction {
    match &runner.state().waiting_for {
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        other => panic!(
            "unexpected prompt in phase {:?}: {other:?}",
            runner.state().phase
        ),
    }
}

/// Drive to the completion boundary every row asserts at: the postcombat main
/// phase with an empty stack, no live combat, and a player holding priority.
fn settle_to_postcombat_main(runner: &mut GameRunner) {
    for _ in 0..400 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain
            && state.stack.is_empty()
            && state.combat.is_none()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        let action = advance_action(runner);
        runner.act(action).unwrap_or_else(|e| {
            panic!(
                "advancing to the postcombat main phase failed: {e:?} (phase = {:?})",
                runner.state().phase
            )
        });
    }
    panic!(
        "never reached the postcombat main phase; phase = {:?}, waiting_for = {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

/// Assert each creature is still on the battlefield with no damage marked.
fn assert_untouched(runner: &GameRunner, creatures: &[(&str, ObjectId)]) {
    for (label, id) in creatures {
        assert_eq!(
            zone(runner, *id),
            Zone::Battlefield,
            "{label} must stay on the battlefield"
        );
        assert_eq!(damage(runner, *id), 0, "{label} must take no damage");
    }
}

/// R1 — CR 509.1g + CR 608.2i: Cathedral Membrane deals 6 damage only to the
/// creature it blocked this combat. An unblocked attacker and a bystander on
/// each side are untouched (at the base they all took 6 and died).
#[test]
fn cathedral_membrane_damages_only_the_creature_it_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 3, 3).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 2, 2).id();
    let attacker_side_bystander = scenario.add_creature(P0, "P0 Bystander", 0, 2).id();
    let defender_side_bystander = scenario.add_creature(P1, "P1 Bystander", 0, 2).id();
    let membrane = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![(membrane, blocked)],
    );
    settle_to_postcombat_main(&mut runner);

    // Reach guard: the Membrane died in combat and its trigger resolved on the
    // creature it blocked.
    assert_eq!(zone(&runner, membrane), Zone::Graveyard);
    assert_eq!(zone(&runner, blocked), Zone::Graveyard);
    assert_eq!(damage(&runner, blocked), 6);

    assert_untouched(
        &runner,
        &[
            ("the unblocked attacker", unblocked),
            ("the attacking player's bystander", attacker_side_bystander),
            ("the defending player's bystander", defender_side_bystander),
        ],
    );
}

/// R2 — the relation is pairwise (blocker → attacker), not the unary "was
/// blocked": a second attacker blocked by a different creature, and that other
/// blocker, are untouched.
#[test]
fn cathedral_membrane_ignores_a_pair_blocked_by_another_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 3, 3).id();
    let other_attacker = scenario.add_creature(P0, "Other Attacker", 0, 2).id();
    let other_blocker = scenario.add_creature(P1, "Other Blocker", 0, 2).id();
    let membrane = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (other_attacker, AttackTarget::Player(P1)),
        ],
        vec![(membrane, blocked), (other_blocker, other_attacker)],
    );
    settle_to_postcombat_main(&mut runner);

    // Reach guard: the Membrane's trigger dealt its 6 damage.
    assert_eq!(zone(&runner, membrane), Zone::Graveyard);
    assert_eq!(damage(&runner, blocked), 6);

    assert_untouched(
        &runner,
        &[
            ("the attacker blocked by another creature", other_attacker),
            ("the other blocker", other_blocker),
        ],
    );
}

/// R3 — CR 500.8 + CR 511.3: "this combat" is the current combat phase only.
/// With Full Throttle's two extra combats, the Membrane blocks A in combat 1
/// and B in combat 2, dying in combat 2: only B takes the 6 damage.
#[test]
fn cathedral_membrane_this_combat_excludes_an_earlier_combat_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_combat_attacker = scenario.add_creature(P0, "Combat 1 Attacker", 0, 3).id();
    let second_combat_attacker = scenario.add_creature(P0, "Combat 2 Attacker", 3, 3).id();
    let membrane = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let throttle = scenario
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().extra_phases.len(),
        2,
        "reach guard: Full Throttle must schedule two extra combats"
    );

    // Full Throttle cast in the precombat main phase yields its two extra
    // combats plus the turn's regular one; attack only in the first two.
    let mut declare_attackers_rounds = 0;
    let mut checked_combat_two = false;
    for _ in 0..800 {
        let state = runner.state();
        if declare_attackers_rounds >= 3
            && state.phase == Phase::PostCombatMain
            && state.stack.is_empty()
            && state.combat.is_none()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                declare_attackers_rounds += 1;
                if declare_attackers_rounds == 2 {
                    // Reach guard: combat 1 recorded the block and left the
                    // Membrane alive and undamaged going into combat 2.
                    assert_eq!(zone(&runner, membrane), Zone::Battlefield);
                    assert_eq!(damage(&runner, membrane), 0);
                    let state = runner.state();
                    let pair = BlockHistoryPair {
                        blocker: ObjectIncarnationRef::from_object(&state.objects[&membrane]),
                        attacker: ObjectIncarnationRef::from_object(
                            &state.objects[&first_combat_attacker],
                        ),
                    };
                    assert!(
                        state.creature_blocked_attackers_this_turn.contains(&pair),
                        "reach guard: the turn ledger must hold combat 1's block"
                    );
                    checked_combat_two = true;
                }
                let attacks = match declare_attackers_rounds {
                    1 => vec![(first_combat_attacker, AttackTarget::Player(P1))],
                    2 => vec![(second_combat_attacker, AttackTarget::Player(P1))],
                    _ => vec![],
                };
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                let assignments = match declare_attackers_rounds {
                    1 => vec![(membrane, first_combat_attacker)],
                    2 => vec![(membrane, second_combat_attacker)],
                    _ => vec![],
                };
                runner
                    .act(GameAction::DeclareBlockers { assignments })
                    .expect("declare blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt in phase {:?}: {other:?}", state.phase),
        }
    }

    assert!(
        checked_combat_two,
        "must have reached combat 2's declare-attackers step"
    );
    assert_eq!(
        declare_attackers_rounds, 3,
        "reach guard: all three combats must have run"
    );
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert!(runner.state().stack.is_empty());

    // Reach guard: the Membrane died in combat 2 and hit what it blocked there.
    assert_eq!(zone(&runner, membrane), Zone::Graveyard);
    assert_eq!(zone(&runner, second_combat_attacker), Zone::Graveyard);
    assert_eq!(damage(&runner, second_combat_attacker), 6);

    assert_untouched(
        &runner,
        &[(
            "the creature blocked in an earlier combat",
            first_combat_attacker,
        )],
    );
}

/// R4 — the relation is anchored on each trigger's own source: with two
/// Membranes, the one that dies hits only what it blocked, not what the
/// surviving Membrane blocked.
#[test]
fn cathedral_membrane_is_anchored_on_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lethal_attacker = scenario.add_creature(P0, "Lethal Attacker", 3, 3).id();
    let harmless_attacker = scenario.add_creature(P0, "Harmless Attacker", 0, 3).id();
    let dying_membrane = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let surviving_membrane = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (lethal_attacker, AttackTarget::Player(P1)),
            (harmless_attacker, AttackTarget::Player(P1)),
        ],
        vec![
            (dying_membrane, lethal_attacker),
            (surviving_membrane, harmless_attacker),
        ],
    );
    settle_to_postcombat_main(&mut runner);

    // Reach guard: the dying Membrane's trigger resolved on its own blockee.
    assert_eq!(zone(&runner, dying_membrane), Zone::Graveyard);
    assert_eq!(zone(&runner, lethal_attacker), Zone::Graveyard);
    assert_eq!(damage(&runner, lethal_attacker), 6);

    assert_untouched(
        &runner,
        &[
            ("the creature the other Membrane blocked", harmless_attacker),
            ("the surviving Membrane", surviving_membrane),
        ],
    );
}

/// R5 — CR 701.8a + CR 608.2i: Defiant Vanguard destroys itself and only the
/// creature it blocked this turn. The blocked attacker survives combat damage,
/// so its destruction proves the trigger reached it; the unblocked attacker and
/// both bystanders survive (at the base the trigger destroyed every creature).
#[test]
fn defiant_vanguard_destroys_itself_and_only_what_it_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 1, 4).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 1, 1).id();
    let attacker_side_bystander = scenario.add_creature(P0, "P0 Bystander", 0, 2).id();
    let defender_side_bystander = scenario.add_creature(P1, "P1 Bystander", 0, 2).id();
    let vanguard = scenario
        .add_creature_from_oracle(P1, "Defiant Vanguard", 2, 2, DEFIANT_VANGUARD)
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![(vanguard, blocked)],
    );
    settle_to_postcombat_main(&mut runner);

    // Reach guard: 2 combat damage is below the attacker's toughness of 4, so
    // only the end-of-combat trigger can have put it in the graveyard.
    assert_eq!(damage(&runner, blocked), 2);
    assert_eq!(zone(&runner, blocked), Zone::Graveyard);
    assert_eq!(
        zone(&runner, vanguard),
        Zone::Graveyard,
        "\"destroy it\" names Defiant Vanguard itself"
    );

    for (label, id) in [
        ("the unblocked attacker", unblocked),
        ("the attacking player's bystander", attacker_side_bystander),
        ("the defending player's bystander", defender_side_bystander),
    ] {
        assert_eq!(
            zone(&runner, id),
            Zone::Battlefield,
            "{label} must not be destroyed"
        );
    }
}

/// R5b — CR 603.7e + CR 113.7a: when Defiant Vanguard dies in combat damage
/// before its end-of-combat trigger, the delayed trigger still destroys the
/// creature it blocked this turn — and nothing else.
#[test]
fn defiant_vanguard_trigger_still_finds_its_blockee_after_it_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 3, 4).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 1, 1).id();
    let defender_side_bystander = scenario.add_creature(P1, "P1 Bystander", 0, 2).id();
    let vanguard = scenario
        .add_creature_from_oracle(P1, "Defiant Vanguard", 2, 2, DEFIANT_VANGUARD)
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![(vanguard, blocked)],
    );

    // Reach guard: Defiant Vanguard is already in the graveyard when the end of
    // combat step begins.
    for _ in 0..100 {
        if runner.state().phase == Phase::EndCombat {
            break;
        }
        let action = advance_action(&runner);
        runner.act(action).expect("advance to end of combat");
    }
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert_eq!(zone(&runner, vanguard), Zone::Graveyard);

    settle_to_postcombat_main(&mut runner);

    assert_eq!(damage(&runner, blocked), 2);
    assert_eq!(
        zone(&runner, blocked),
        Zone::Graveyard,
        "the delayed trigger must still destroy the creature Vanguard blocked"
    );
    for (label, id) in [
        ("the unblocked attacker", unblocked),
        ("the defending player's bystander", defender_side_bystander),
    ] {
        assert_eq!(
            zone(&runner, id),
            Zone::Battlefield,
            "{label} must not be destroyed"
        );
    }
}

const MURDER: &str = "Destroy target creature.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";

/// Cast `spell` (already in its owner's hand) at `target` while the blocks
/// trigger waits on the stack, then pass priority until only that trigger
/// remains (CR 117.4): the response resolves before the delayed trigger is
/// created.
fn respond_before_blocks_trigger_resolves(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
) {
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the blocks trigger waits on the stack"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "no delayed trigger exists yet"
    );
    {
        let mut commit = runner.cast(spell).target_object(target).commit();
        assert_eq!(commit.state().stack.len(), 2);
        commit.act(GameAction::PassPriority).expect("pass");
        commit.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the response resolved; the blocks trigger remains"
    );
    assert!(runner.state().delayed_triggers.is_empty());
}

/// R5c: CR 113.7a + CR 603.7e + CR 400.7: Defiant Vanguard is destroyed while its
/// blocks trigger is still on the stack. The trigger still resolves and
/// creates the end-of-combat trigger, whose source is the Vanguard that blocked,
/// so "all creatures it blocked this turn" still names the blocked attacker.
#[test]
fn defiant_vanguard_destroyed_before_its_trigger_resolves_still_destroys_its_blockee() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 1, 4).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 1, 1).id();
    let defender_side_bystander = scenario.add_creature(P1, "P1 Bystander", 0, 2).id();
    let vanguard = scenario
        .add_creature_from_oracle(P1, "Defiant Vanguard", 2, 2, DEFIANT_VANGUARD)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![(vanguard, blocked)],
    );
    respond_before_blocks_trigger_resolves(&mut runner, murder, vanguard);
    assert_eq!(zone(&runner, vanguard), Zone::Graveyard);

    // The blocks trigger resolves (CR 113.7a) and creates the delayed trigger.
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the delayed trigger was created"
    );

    settle_to_postcombat_main(&mut runner);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the delayed trigger fired"
    );
    // Reach guard: the blocker left before combat damage, so the attacker took none.
    assert_eq!(damage(&runner, blocked), 0);
    assert_eq!(
        zone(&runner, blocked),
        Zone::Graveyard,
        "the delayed trigger must still destroy the creature Vanguard blocked"
    );
    assert_eq!(zone(&runner, vanguard), Zone::Graveyard);
    for (label, id) in [
        ("the unblocked attacker", unblocked),
        ("the defending player's bystander", defender_side_bystander),
    ] {
        assert_eq!(
            zone(&runner, id),
            Zone::Battlefield,
            "{label} must not be destroyed"
        );
    }
}

/// R5d: CR 400.7 + CR 603.7e + CR 113.7a: Defiant Vanguard is flickered while
/// its blocks trigger is on the stack. The Vanguard that returns is a new
/// object, so the delayed trigger keeps the Vanguard that blocked as its
/// source: "all creatures it blocked this turn" still names what that Vanguard
/// blocked, and "destroy it" does not reach the new Vanguard (CR 603.7c).
#[test]
fn defiant_vanguard_flickered_before_its_trigger_resolves_still_destroys_its_blockee() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 1, 4).id();
    let vanguard = scenario
        .add_creature_from_oracle(P1, "Defiant Vanguard", 2, 2, DEFIANT_VANGUARD)
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(blocked, AttackTarget::Player(P1))],
        vec![(vanguard, blocked)],
    );
    // The active player passes; the defending player responds.
    runner.act(GameAction::PassPriority).expect("pass");
    let before = runner.state().objects[&vanguard].incarnation;
    respond_before_blocks_trigger_resolves(&mut runner, cloudshift, vanguard);
    assert_eq!(zone(&runner, vanguard), Zone::Battlefield);
    assert_ne!(
        runner.state().objects[&vanguard].incarnation,
        before,
        "it is a new object"
    );

    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    settle_to_postcombat_main(&mut runner);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the delayed trigger fired"
    );
    assert_eq!(
        zone(&runner, blocked),
        Zone::Graveyard,
        "the creature the original Vanguard blocked is destroyed"
    );
    assert_eq!(
        zone(&runner, vanguard),
        Zone::Battlefield,
        "the returned Vanguard is a new object and is not destroyed"
    );
}

/// One blocker with a self-subject block trigger blocks an attacker that its
/// power cannot kill (1/4), then the game settles at the postcombat main phase.
/// Returns the runner and the (blocker, attacker) ids.
fn self_blocker_scenario(
    name: &str,
    power: i32,
    toughness: i32,
    oracle: &str,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 1, 4).id();
    let blocker = scenario
        .add_creature_from_oracle(P1, name, power, toughness, oracle)
        .id();
    let mut runner = scenario.build();
    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![(blocker, attacker)],
    );
    (runner, blocker, attacker)
}

/// R6 — CR 608.2k: Cinder Wall's "destroy it at end of combat" destroys the
/// Wall, not the creature it blocked.
#[test]
fn cinder_wall_destroys_itself_not_the_creature_it_blocked() {
    let (mut runner, wall, attacker) = self_blocker_scenario("Cinder Wall", 0, 3, CINDER_WALL);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, wall), Zone::Graveyard);
    assert_eq!(zone(&runner, attacker), Zone::Battlefield);
}

/// R7 — CR 608.2k: Wall of Junk returns itself to its owner's hand, not the
/// creature it blocked.
#[test]
fn wall_of_junk_returns_itself_not_the_creature_it_blocked() {
    let (mut runner, wall, attacker) = self_blocker_scenario("Wall of Junk", 0, 3, WALL_OF_JUNK);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, wall), Zone::Hand);
    assert_eq!(runner.state().objects[&wall].owner, P1);
    assert_eq!(zone(&runner, attacker), Zone::Battlefield);
}

/// R8 (b) — CR 608.2k: a blocking Ceremonial Guard destroys itself, not the
/// creature it blocked.
#[test]
fn ceremonial_guard_blocking_destroys_itself() {
    let (mut runner, guard, attacker) =
        self_blocker_scenario("Ceremonial Guard", 0, 3, CEREMONIAL_GUARD);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, guard), Zone::Graveyard);
    assert_eq!(zone(&runner, attacker), Zone::Battlefield);
}

/// R8 (a) — preservation: an attacking Ceremonial Guard destroys itself and
/// leaves its blocker alone.
#[test]
fn ceremonial_guard_attacking_destroys_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let guard = scenario
        .add_creature_from_oracle(P0, "Ceremonial Guard", 3, 4, CEREMONIAL_GUARD)
        .id();
    let blocker = scenario.add_creature(P1, "Blocker", 1, 4).id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![(guard, AttackTarget::Player(P1))],
        vec![(blocker, guard)],
    );
    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, guard), Zone::Graveyard);
    assert_eq!(zone(&runner, blocker), Zone::Battlefield);
}

/// R9 — CR 608.2k on the immediate (non-delayed) path: Zephyr Spirit's block
/// trigger returns the Spirit itself to its owner's hand.
#[test]
fn zephyr_spirit_returns_itself_not_the_creature_it_blocked() {
    let (mut runner, spirit, attacker) =
        self_blocker_scenario("Zephyr Spirit", 0, 6, ZEPHYR_SPIRIT);

    // Reach guard: the block trigger is on the stack right after blockers are
    // declared.
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1, "the block trigger must be on the stack");
    assert_eq!(stack[0].source_id, spirit);

    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, spirit), Zone::Hand);
    assert_eq!(runner.state().objects[&spirit].owner, P1);
    assert_eq!(zone(&runner, attacker), Zone::Battlefield);
}

/// Block with `name`, let the blocks trigger resolve so its end-of-combat
/// delayed trigger exists, then have the defending player Cloudshift the
/// blocker (CR 400.7: the permanent that returns is a new object). Advance to
/// the end of combat and stop with the delayed trigger on the stack. Returns
/// the runner, the blocker and attacker ids, and the returned blocker's
/// incarnation.
fn blink_blocker_after_its_delayed_trigger_exists(
    name: &str,
    oracle: &str,
) -> (GameRunner, ObjectId, ObjectId, u64) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 1, 4).id();
    let blocker = scenario
        .add_creature_from_oracle(P1, name, 0, 3, oracle)
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    drive_declare_blockers(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![(blocker, attacker)],
    );

    // The blocks trigger resolves (CR 603.7a) and creates the delayed trigger.
    assert_eq!(runner.state().stack.len(), 1, "the blocks trigger waits");
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the end-of-combat trigger exists before the blink"
    );

    // The active player passes; the defending player Cloudshifts the blocker.
    runner.act(GameAction::PassPriority).expect("pass");
    let blocked_as = runner.state().objects[&blocker].incarnation;
    {
        let mut commit = runner.cast(cloudshift).target_object(blocker).commit();
        commit.act(GameAction::PassPriority).expect("pass");
        commit.act(GameAction::PassPriority).expect("pass");
    }
    assert!(runner.state().stack.is_empty(), "Cloudshift resolved");
    assert_eq!(zone(&runner, blocker), Zone::Battlefield);
    let returned_as = runner.state().objects[&blocker].incarnation;
    assert_ne!(
        returned_as, blocked_as,
        "the returned blocker is a new object"
    );

    // Advance to the end of combat step, where the delayed trigger fires.
    for _ in 0..200 {
        if runner.state().phase == Phase::EndCombat && !runner.state().stack.is_empty() {
            break;
        }
        let action = advance_action(&runner);
        runner.act(action).expect("advance to the end of combat");
    }
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1, "the delayed trigger fired at end of combat");
    assert_eq!(stack[0].source_id, blocker, "its source is the blocker");
    assert!(runner.state().delayed_triggers.is_empty());
    (runner, blocker, attacker, returned_as)
}

/// R10 — CR 603.7c + CR 400.7: Cinder Wall blocks, its end-of-combat "destroy
/// it" is created, then the Wall is Cloudshifted. The Wall that returned is a
/// new object, so the delayed trigger destroys nothing: the returned Wall and
/// the attacker it blocked both survive. (R6 is the unchanged-Wall control.)
#[test]
fn cinder_wall_blinked_after_its_delayed_trigger_exists_survives() {
    let (mut runner, wall, attacker, returned_as) =
        blink_blocker_after_its_delayed_trigger_exists("Cinder Wall", CINDER_WALL);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(
        zone(&runner, wall),
        Zone::Battlefield,
        "the returned Wall must not be destroyed"
    );
    assert_eq!(runner.state().objects[&wall].incarnation, returned_as);
    assert_eq!(
        zone(&runner, attacker),
        Zone::Battlefield,
        "the creature the Wall blocked must not be destroyed"
    );
}

/// R11 — CR 603.7c + CR 400.7: the same blink after Ceremonial Guard's
/// delayed "destroy it" exists. (R8 (b) is the unchanged-Guard control.)
#[test]
fn ceremonial_guard_blinked_after_its_delayed_trigger_exists_survives() {
    let (mut runner, guard, attacker, returned_as) =
        blink_blocker_after_its_delayed_trigger_exists("Ceremonial Guard", CEREMONIAL_GUARD);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, guard), Zone::Battlefield);
    assert_eq!(runner.state().objects[&guard].incarnation, returned_as);
    assert_eq!(zone(&runner, attacker), Zone::Battlefield);
}

/// R12 — CR 603.7c + CR 400.7 + CR 113.7a: Defiant Vanguard is blinked after
/// its delayed trigger exists. "Destroy it" no longer reaches the returned
/// Vanguard, while "all creatures it blocked this turn" still names the
/// creature the original Vanguard blocked.
#[test]
fn defiant_vanguard_blinked_after_its_delayed_trigger_exists_destroys_only_its_blockee() {
    let (mut runner, vanguard, blocked, returned_as) =
        blink_blocker_after_its_delayed_trigger_exists("Defiant Vanguard", DEFIANT_VANGUARD);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(
        zone(&runner, vanguard),
        Zone::Battlefield,
        "the returned Vanguard must not be destroyed"
    );
    assert_eq!(runner.state().objects[&vanguard].incarnation, returned_as);
    assert_eq!(
        zone(&runner, blocked),
        Zone::Graveyard,
        "the creature the original Vanguard blocked is still destroyed"
    );
}

/// R13 — preservation, CR 603.7c: Wall of Junk's delayed "return it" already
/// skipped a blinked Wall through `ChangeZone`'s own currency check; the
/// returned Wall stays on the battlefield.
#[test]
fn wall_of_junk_blinked_after_its_delayed_trigger_exists_stays() {
    let (mut runner, wall, attacker, returned_as) =
        blink_blocker_after_its_delayed_trigger_exists("Wall of Junk", WALL_OF_JUNK);
    settle_to_postcombat_main(&mut runner);

    assert_eq!(zone(&runner, wall), Zone::Battlefield);
    assert_eq!(runner.state().objects[&wall].incarnation, returned_as);
    assert_eq!(zone(&runner, attacker), Zone::Battlefield);
}
