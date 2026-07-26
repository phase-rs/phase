//! CR733 coverage for the effect-driven combat-membership family.
//!
//! Five authorities put an object into or out of combat by effect rather than by
//! declaration — `combat::enter_attacking`, `combat::place_attacking_alongside`,
//! `combat::place_blocking`, `combat::mark_attacker_blocked`, and
//! `remove_from_combat::remove_object_from_combat` — and every one of them wrote
//! its mutation raw. A retained-prefix replay therefore had no record that a
//! creature was ever attacking, blocking, or blocked by effect.
//!
//! They share ONE parameterized command rather than five sibling variants. The
//! axis stays inside a single CR section, as the categorical boundary rule
//! requires: CR 506.3a-g govern putting a permanent onto the battlefield
//! "attacking or blocking" in one breath, CR 506.4 governs removal, and the
//! declaration-side rules delegate back to it (CR 509.1g ends "See rule 506.4.").
//!
//! The load-bearing reason the defender must be RECORDED rather than re-derived
//! is CR 508.4: "its controller chooses which defending player, planeswalker a
//! defending player controls, or battle a defending player protects it's
//! attacking." That is a choice the rules assign to a player. The resolve-time
//! authority approximates it from ambient state —
//! `defending_player_for_enters_attacking` reads the source's own attacker
//! entry, then `state.current_trigger_event`, then a controller-scan of the live
//! attacker list, then falls back to the first opponent. None of that is
//! reconstructible at replay time, so re-deriving would silently seat the
//! creature against a different defender. `replay_installs_recorded_defender_
//! when_ambient_derivation_diverges` is the test that pins it, and it carries a
//! probe proving the ambient path really would answer differently.

use engine::game::combat::{apply_resolved_combat_membership, AttackTarget, CombatParticipation};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedCombatMembershipCommand, ResolvedCombatMembershipEdit,
    ResolvedCombatMembershipReplayInvariantError, ResolvedRulesCommand,
};

/// Verbatim Oracle text (Scryfall, 2026-07). A paraphrase risks taking a
/// different parser branch than the real card.
const KAALIA_ORACLE: &str = "Flying\nWhenever Kaalia attacks an opponent, you may put an Angel, Demon, or Dragon creature card from your hand onto the battlefield tapped and attacking that opponent.";

/// Every combat-membership command recorded after `journal_start`, in journal
/// (execution) order.
fn membership_commands(
    state: &GameState,
    journal_start: usize,
) -> Vec<ResolvedCombatMembershipCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::CombatMembership(command) => Some(command),
            _ => None,
        })
        .collect()
}

fn attacker_entry(state: &GameState, oid: ObjectId) -> Option<(PlayerId, AttackTarget, bool)> {
    state.combat.as_ref().and_then(|combat| {
        combat
            .attackers
            .iter()
            .find(|a| a.object_id == oid)
            .map(|a| (a.defending_player, a.attack_target, a.blocked))
    })
}

/// Advance from a main-phase priority to the declare-attackers step.
fn advance_to_declare_attackers(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare attackers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering combat triggers should succeed");
            }
            other => panic!("unexpected waiting_for advancing to declare attackers: {other:?}"),
        }
    }
    panic!("expected DeclareAttackers");
}

/// A Kaalia attack that has resolved its "put a creature onto the battlefield
/// tapped and attacking that opponent" trigger.
struct KaaliaAttack {
    runner: GameRunner,
    kaalia: ObjectId,
    angel: ObjectId,
    /// A bystander creature used by the L1 probe to observe what the LIVE
    /// ambient derivation answers in the divergent replay state.
    control: ObjectId,
    journal_start: usize,
}

/// Drives the REAL pipeline: declare attackers, then let Kaalia's attack trigger
/// resolve through the production stack so `change_zone` reaches
/// `combat::enter_attacking` exactly as it does in a game.
fn kaalia_attack() -> KaaliaAttack {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kaalia = scenario
        .add_creature_from_oracle(P0, "Kaalia of the Vast", 2, 2, KAALIA_ORACLE)
        .id();
    let angel = scenario
        .add_creature_to_hand(P0, "Serra Angel", 4, 4)
        .with_subtypes(vec!["Angel"])
        .id();
    let control = scenario.add_vanilla(P0, 1, 1);

    let mut runner = scenario.build();
    advance_to_declare_attackers(&mut runner, P0);

    let journal_start = runner.state().resolved_rules_journal.entries().len();

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(kaalia, AttackTarget::Player(P1))],
            bands: Vec::new(),
        })
        .expect("Kaalia can attack the only opponent");

    // Resolve the attack trigger, answering the "you may" and the card choice.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering the attack trigger should succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Kaalia's may-clause should succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should resolve the attack trigger");
            }
            other => panic!("unexpected waiting_for resolving Kaalia's trigger: {other:?}"),
        }
    }

    KaaliaAttack {
        runner,
        kaalia,
        angel,
        control,
        journal_start,
    }
}

/// Headline: a creature put onto the battlefield attacking is journaled as an
/// exact resolved command, and replaying that command reproduces the attacking
/// entry against the RECORDED defender.
#[test]
fn enters_attacking_journals_and_replays_its_recorded_defender() {
    let attack = kaalia_attack();
    let state = attack.runner.state();

    // Reach guards. Without these the journal assertion below could pass
    // vacuously on a trigger that never resolved, or on an Angel that entered
    // the battlefield without ever becoming an attacking creature.
    assert_eq!(
        state.objects[&attack.angel].zone,
        engine::types::zones::Zone::Battlefield,
        "the trigger must have put the Angel onto the battlefield"
    );
    let (defender, target, _) = attacker_entry(state, attack.angel)
        .expect("CR 508.4: the Angel must be an attacking creature");
    assert_eq!(
        defender, P1,
        "CR 508.4: the Angel attacks the opponent Kaalia attacked"
    );
    assert_eq!(target, AttackTarget::Player(P1));

    // Discriminating assertion: the membership edit is journaled. A raw
    // mutation records nothing here.
    let commands: Vec<_> = membership_commands(state, attack.journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == attack.angel)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the attacking authority must journal exactly one resolved edit for the Angel"
    );
    let command = &commands[0];
    assert_eq!(
        command.edit,
        ResolvedCombatMembershipEdit::Attack {
            resulting_defending_player: P1,
            resulting_attack_target: AttackTarget::Player(P1),
        },
        "the recorded pair is the CR 508.4 choice the authority settled"
    );

    // Replay exactness. Fields are compared individually: `CombatState`'s
    // hand-written `PartialEq` skips `damage_assignments`, `damage_step_index`,
    // `pending_damage`, and `regular_damage_done`, so a whole-struct equality
    // assertion would be blind to divergence in exactly those fields.
    //
    // The predecessor is the post-resolution state with ONLY this family's edit
    // undone. It cannot be the pre-declaration state: the Angel moved from hand
    // to battlefield, and CR 400.7 makes that a NEW object, so the recorded
    // incarnation would legitimately not match (the applier rejects that with
    // `StaleObject`, which the fail-closed test relies on).
    let mut replay = state.clone();
    if let Some(combat) = replay.combat.as_mut() {
        combat.attackers.retain(|a| a.object_id != attack.angel);
    }
    apply_resolved_combat_membership(&mut replay, command)
        .expect("the recorded edit must replay against a state without the Angel attacking");
    let (replay_defender, replay_target, replay_blocked) = attacker_entry(&replay, attack.angel)
        .expect("replay must reinstate the Angel as an attacking creature");
    assert_eq!(
        replay_defender, P1,
        "replay installs the recorded defending player"
    );
    assert_eq!(
        replay_target,
        AttackTarget::Player(P1),
        "replay installs the recorded attack target"
    );
    assert!(
        !replay_blocked,
        "CR 509.1h: a creature entering attacking is not blocked"
    );
}

/// The point of the unit (landmine L1). `enter_attacking` picks its defender
/// from AMBIENT state, so the applier must never call it. This drives replay
/// into a state whose ambient derivation demonstrably answers DIFFERENTLY, and
/// proves the recorded defender still wins.
///
/// CR 508.4 makes this a rules question, not just a determinism question: the
/// defender is a choice the controller already made, and re-deriving re-decides
/// a settled choice.
#[test]
fn replay_installs_recorded_defender_when_ambient_derivation_diverges() {
    let attack = kaalia_attack();
    let state = attack.runner.state();
    let command = membership_commands(state, attack.journal_start)
        .into_iter()
        .find(|command| command.object.object_id == attack.angel)
        .expect("the Angel's attacking entry must be journaled");

    // Build a replay state where every ambient source disagrees with the
    // record: Kaalia is no longer an attacker (defeating both the source lookup
    // and the controller-scan), and the current trigger event names P0.
    let mut replay = state.clone();
    if let Some(combat) = replay.combat.as_mut() {
        combat
            .attackers
            .retain(|a| a.object_id != attack.angel && a.object_id != attack.kaalia);
    }
    replay.current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: attack.kaalia,
        target: TargetRef::Player(P0),
        amount: 1,
        is_combat: false,
        excess: 0,
    });

    // Non-vacuity probe, VERIFIED APPLIED: run the live authority in this exact
    // state on a control creature. If it still derived P1 the test below would
    // be vacuous — a re-deriving applier would coincidentally look correct.
    let mut probe = replay.clone();
    engine::game::combat::enter_attacking(&mut probe, attack.control, attack.kaalia, P0);
    let (probe_defender, _, _) = attacker_entry(&probe, attack.control)
        .expect("the probe creature must be seated as an attacker");
    assert_eq!(
        probe_defender, P0,
        "probe: ambient derivation in this state must answer P0, otherwise the \
         divergence assertion below is vacuous"
    );

    // The discriminating assertion: replay ignores ambient state entirely.
    apply_resolved_combat_membership(&mut replay, &command)
        .expect("the recorded edit must replay regardless of ambient combat state");
    let (replay_defender, replay_target, _) = attacker_entry(&replay, attack.angel)
        .expect("replay must reinstate the Angel as an attacking creature");
    assert_eq!(
        replay_defender, P1,
        "CR 508.4: replay installs the RECORDED defender, not the ambient one"
    );
    assert_eq!(
        replay_target,
        AttackTarget::Player(P1),
        "replay installs the recorded attack target, not the ambient one"
    );
}

/// Fail-closed: an `expected_*` that no longer describes live state must return
/// a typed error instead of installing the `resulting_*` anyway. CR 506.3c and
/// CR 508.4a are the rules reason — a creature whose recorded defender is gone
/// is never an attacking creature, so silently seating it elsewhere is wrong.
#[test]
fn replay_fails_closed_when_the_recorded_precondition_no_longer_holds() {
    let attack = kaalia_attack();
    let state = attack.runner.state();
    let command = membership_commands(state, attack.journal_start)
        .into_iter()
        .find(|command| command.object.object_id == attack.angel)
        .expect("the Angel's attacking entry must be journaled");

    // The Angel is ALREADY attacking in this state, so the command's
    // "not yet an attacker" precondition is violated.
    let mut replay = state.clone();
    assert!(
        attacker_entry(&replay, attack.angel).is_some(),
        "reach guard: the Angel must already be attacking for this to be a real conflict"
    );
    let error = apply_resolved_combat_membership(&mut replay, &command)
        .expect_err("replaying onto an already-attacking creature must fail closed");
    assert_eq!(
        error,
        ResolvedCombatMembershipReplayInvariantError::AlreadyAttacking(attack.angel),
        "the failure is typed, not a silent duplicate attacker"
    );
    // The rejection left no partial edit.
    assert_eq!(
        replay.combat.as_ref().map(|combat| combat
            .attackers
            .iter()
            .filter(|a| a.object_id == attack.angel)
            .count()),
        Some(1),
        "a rejected command must not have pushed a second attacker entry"
    );
}

/// CR 506.4 removal: the authority records the exact roles it pruned, replay
/// reproduces the prune, and a participation mismatch fails closed.
///
/// `damage_assignments` is carried in the receipt precisely because
/// `CombatState`'s `PartialEq` does not compare it.
#[test]
fn removal_journals_exact_participation_and_replays_it() {
    let attack = kaalia_attack();
    let mut runner = attack.runner;

    let before_removal = runner.state().clone();
    let journal_start = before_removal.resolved_rules_journal.entries().len();
    let expected = CombatParticipation::capture(&before_removal, attack.angel);

    // Reach guard: the object really is in combat, so the removal below prunes
    // something and the journal assertion cannot pass vacuously.
    assert!(
        !expected.is_empty(),
        "reach guard: the Angel must hold a combat role before removal"
    );
    assert!(
        expected.attacking.is_some(),
        "reach guard: the Angel is the attacking creature being pruned"
    );

    engine::game::effects::remove_from_combat::remove_object_from_combat(
        runner.state_mut(),
        attack.angel,
    );

    assert!(
        attacker_entry(runner.state(), attack.angel).is_none(),
        "CR 506.4: the removed creature stops being an attacking creature"
    );

    let commands: Vec<_> = membership_commands(runner.state(), journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == attack.angel)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the removal authority must journal exactly one resolved edit"
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Remove {
            expected_participation: expected.clone(),
        },
        "the receipt records the exact roles the removal pruned"
    );

    // Replay exactness against the captured predecessor.
    let mut replay = before_removal.clone();
    apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect("the recorded removal must replay against its captured predecessor");
    assert!(
        attacker_entry(&replay, attack.angel).is_none(),
        "replay reproduces the prune"
    );
    assert_eq!(
        CombatParticipation::capture(&replay, attack.angel),
        CombatParticipation::default(),
        "replay leaves the object holding no combat role at all"
    );

    // Fail-closed: the same command against a state where the object no longer
    // participates must be rejected rather than pruning nothing silently.
    let error = apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect_err("re-applying a removal to an unparticipating object must fail closed");
    assert!(
        matches!(
            error,
            ResolvedCombatMembershipReplayInvariantError::ParticipationMismatch { .. }
        ),
        "the failure is a typed participation mismatch, got {error:?}"
    );
}

/// Verbatim Oracle text (Scryfall, 2026-07).
const DAZZLING_BEAUTY: &str = "Cast this spell only during the declare blockers step.\nTarget unblocked attacking creature becomes blocked.";

/// Verbatim Oracle text (Scryfall, 2026-07).
const MIRROR_MATCH: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";

/// Drive P0's attack into the declare-blockers step and hand priority to P1 so
/// the defending player can cast during that step (CR 509).
fn advance_to_declare_blockers_and_give_priority(runner: &mut GameRunner, caster: PlayerId) {
    for _ in 0..20 {
        if runner.state().phase == Phase::DeclareBlockers {
            let state = runner.state_mut();
            state.priority_player = caster;
            state.waiting_for = WaitingFor::Priority { player: caster };
            return;
        }
        let wf = runner.state().waiting_for.clone();
        let acted = match wf {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            other => panic!("unexpected waiting state before declare-blockers: {other:?}"),
        };
        acted.expect("combat advance action");
    }
    panic!("did not reach the declare-blockers step");
}

/// CR 509.1h via `mark_attacker_blocked`: an attacker made blocked purely by
/// effect, with no blocker assigned. Driven through a real Dazzling Beauty cast.
#[test]
fn become_blocked_journals_a_mark_blocked_edit_and_replays_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Dazzling Beauty", true, DAZZLING_BEAUTY)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_to_declare_blockers_and_give_priority(&mut runner, P1);

    // Reach guard: the attacker must be an UNBLOCKED attacker before the cast,
    // otherwise the mark below is a no-op and is never journaled.
    let (_, _, blocked_before) =
        attacker_entry(runner.state(), attacker).expect("the Ox must be attacking");
    assert!(
        !blocked_before,
        "reach guard: the attacker must start unblocked"
    );

    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let before = runner.state().clone();
    runner.cast(spell).target_objects(&[attacker]).resolve();

    // Reach guard: the effect actually took hold.
    let (_, _, blocked_after) =
        attacker_entry(runner.state(), attacker).expect("the Ox must still be attacking");
    assert!(
        blocked_after,
        "CR 509.1h: the resolved spell must make the attacker blocked"
    );

    let commands: Vec<_> = membership_commands(runner.state(), journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == attacker)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the mark-blocked authority must journal exactly one resolved edit"
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::MarkBlocked,
        "CR 509.1h: the recorded edit is the effect-driven blocked mark"
    );

    // Replay exactness against the pre-cast state.
    let mut replay = before;
    apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect("the recorded mark must replay against its captured predecessor");
    let (_, _, replayed_blocked) =
        attacker_entry(&replay, attacker).expect("replay keeps the Ox attacking");
    assert!(replayed_blocked, "replay installs the blocked bit");
    // CR 509.1h: marking blocked assigns NO blocker, so the maps stay empty.
    let participation = CombatParticipation::capture(&replay, attacker);
    assert!(
        participation.blocked_by.is_empty(),
        "CR 510.1c: an effect-blocked attacker has no creatures blocking it"
    );

    // Fail-closed: the bit is sticky, so re-applying must be rejected rather
    // than silently re-marking.
    let error = apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect_err("re-marking an already-blocked attacker must fail closed");
    assert!(
        matches!(
            error,
            ResolvedCombatMembershipReplayInvariantError::BlockedPreconditionMismatch { .. }
        ),
        "the failure is a typed blocked-precondition mismatch, got {error:?}"
    );
}

/// CR 509.1g + CR 506.3e via `place_blocking`: a token put onto the battlefield
/// already blocking. Driven through a real Mirror Match cast.
///
/// L3: the authority writes FOUR places — the attacker's sticky `blocked` bit,
/// `blocker_to_attacker`, `blocker_assignments`, and `creatures_blocked_this_turn`
/// (which lives on `GameState`, not `CombatState`). All four are asserted after
/// replay, individually.
#[test]
fn place_blocking_journals_a_block_edit_and_replays_all_four_writes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_to_declare_blockers_and_give_priority(&mut runner, P1);

    let journal_start = runner.state().resolved_rules_journal.entries().len();
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: a copy token exists and is genuinely blocking the attacker.
    let state = runner.state();
    let combat = state.combat.as_ref().expect("combat is live");
    let blockers = combat
        .blocker_assignments
        .get(&attacker)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        blockers.len(),
        1,
        "CR 509.1g: Mirror Match must put exactly one copy token in as a blocker"
    );
    let token = blockers[0];
    assert!(
        combat
            .blocker_to_attacker
            .get(&token)
            .is_some_and(|a| a.contains(&attacker)),
        "reach guard: the reverse lookup must name the attacker"
    );

    let commands: Vec<_> = membership_commands(state, journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == token)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the blocking authority must journal exactly one resolved edit for the token"
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Block {
            resulting_attacker: attacker,
            expected_attacker_blocked: false,
        },
        "CR 509.1h: the token is the FIRST blocker, so the sticky bit was clear"
    );

    // Replay exactness: undo only this family's edit, then reinstall it.
    let mut replay = state.clone();
    if let Some(combat) = replay.combat.as_mut() {
        combat.blocker_assignments.remove(&attacker);
        combat.blocker_to_attacker.remove(&token);
        for info in combat.attackers.iter_mut() {
            if info.object_id == attacker {
                info.blocked = false;
            }
        }
    }
    replay.creatures_blocked_this_turn.remove(&token);

    apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect("the recorded block must replay against its predecessor");

    // All four writes, asserted individually. `CombatState`'s hand-written
    // `PartialEq` omits four fields, so a whole-struct equality check here
    // would be blind to exactly the bookkeeping this family edits.
    let replayed = replay.combat.as_ref().expect("combat is live after replay");
    assert!(
        replayed
            .attackers
            .iter()
            .any(|a| a.object_id == attacker && a.blocked),
        "write 1 — CR 509.1h: the attacker's sticky blocked bit"
    );
    assert_eq!(
        replayed.blocker_to_attacker.get(&token),
        Some(&vec![attacker]),
        "write 2 — CR 509.1g: the blocker -> attacker reverse lookup"
    );
    assert_eq!(
        replayed.blocker_assignments.get(&attacker),
        Some(&vec![token]),
        "write 3 — CR 509.1g: the attacker -> blocker forward assignment"
    );
    assert!(
        replay.creatures_blocked_this_turn.contains(&token),
        "write 4 — CR 509.1a: the per-turn blocked-this-turn set on GameState"
    );
}
