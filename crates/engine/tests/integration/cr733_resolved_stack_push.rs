//! CR733 P2 coverage for objects landing on the stack.
//!
//! Two authorities feed one command. `stack::push_to_stack` is CR 405.1 /
//! CR 601.2a; `stack::push_copy_to_stack` is CR 707.10. They are journaled as a
//! single `ResolvedStackPushCommand` parameterized by `ResolvedStackPushOrigin`
//! rather than as two siblings, because both journal *after* every
//! source-referential stamp has been written into the entry — so both record the
//! same operand set (the finished entry plus its CR 405.2 position) and differ
//! only in which rule to cite.
//!
//! The load-bearing fixture is Auriok Siege Sled, whose activated ability is
//! `Effect::ForceBlock { attacker: Some(Source) }`. That is the one input shape
//! that reaches `bind_force_block_source_recursive`, so it is the only fixture
//! that can tell "journal after stamping" apart from "journal before stamping".
//! A plain spell is degenerate here: with no back face on the source and no
//! force-block referent to bind, the authority stamps nothing and the journal
//! point is unobservable. Do not replace it with a simpler card.
//!
//! Every replay below applies a recorded push to a prefix containing no stack
//! POP, and the `StackDepthMismatch` assertions here are about a push applied
//! TWICE — a push is not idempotent — not about crossing a removal.
//!
//! The CR 405.2 top-of-stack pop IS journaled now
//! (`cr733_resolved_stack_pop.rs`), so a replay may cross one by applying the
//! recorded pop first; `a_recorded_pop_unblocks_a_later_push_replay` in that
//! file demonstrates exactly that against a predecessor where the push alone
//! still fails. Removals that remain un-journaled (remove-at-index and
//! retain-by-predicate, CR 701.6a and the CR 800.4a elimination sweep) still
//! make a crossing replay fail closed by design — the fix there is to journal
//! them, never to relax this applier's precondition.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::stack::apply_resolved_stack_push;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedStackPushCommand, ResolvedStackPushOrigin,
    ResolvedStackPushReplayInvariantError,
};

// Verbatim Oracle text (Scryfall, 2026-07-25). The first ability is the one
// under test; "this creature" is the self-reference the force-block parser
// lowers to `ForceBlockAttackerRef::Source`.
const AURIOK_SIEGE_SLED: &str = "{1}: Target artifact creature blocks this creature this turn if \
                                 able.\n{1}: Target artifact creature can't block this creature \
                                 this turn.";

// Real Twincast (M19 etc.), verbatim.
const TWINCAST: &str = "Copy target instant or sorcery spell. You may choose new targets for the \
                        copy.";

// A no-target sorcery so the copy pipeline never has to retarget anything.
const ELVISH_TOKEN_SPELL: &str = "Create a 1/1 green Elf Warrior creature token.";

/// Every stack-push command journaled after `from`, in journal order.
fn stack_pushes(state: &GameState, from: usize) -> Vec<ResolvedStackPushCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(from)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::StackPush(command) => Some(*command),
            _ => None,
        })
        .collect()
}

fn make_artifact_creature(runner: &mut GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
    object.base_card_types = object.card_types.clone();
}

/// CR 405.1 + CR 601.2a: a real cast journals one exact `Put` push whose
/// recorded entry is the entry that landed, at the CR 405.2 position it landed
/// at, and replaying that record installs it without re-deriving anything.
#[test]
fn real_cast_journals_an_exact_put_stack_push() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, ELVISH_TOKEN_SPELL)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();
    assert!(
        pre_state.stack.is_empty(),
        "reach-guard: the stack is empty, so the push lands at CR 405.2 position 0"
    );

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the real cast must put the spell on the stack");

    // CR 405.1 reach guard: the spell genuinely landed on the stack, so the
    // journal assertion below cannot pass vacuously.
    let landed = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .expect("CR 405.1: the cast spell is on the stack")
        .clone();

    let pushes = stack_pushes(runner.state(), journal_start);
    let recorded: Vec<_> = pushes
        .iter()
        .filter(|push| push.entry.id == spell)
        .collect();
    assert_eq!(
        recorded.len(),
        1,
        "the stack authority must journal exactly one push for the cast spell"
    );
    let push = recorded[0];
    assert_eq!(
        push.origin,
        ResolvedStackPushOrigin::Put,
        "CR 405.1 / CR 601.2a: a cast spell is a Put, not a Copy"
    );
    assert_eq!(push.entry.id, spell);
    assert_eq!(push.entry.source_id, landed.source_id);
    assert_eq!(push.entry.controller, landed.controller);

    // A cast is two-phase, and this family covers only the first phase.
    // `announce_spell_on_stack` pushes at CR 601.2a with `ability: None` and
    // `actual_mana_spent: 0`; the finalized ability and mana are retagged onto
    // the SAME entry later at CR 601.2i (`casting_costs.rs`, "Update the
    // existing stack entry (pushed at announcement)"). So the record is the
    // announcement snapshot, and the live entry has since moved on. Asserting
    // both halves pins the seam instead of letting a future reader assume the
    // record is the finalized spell.
    assert!(
        matches!(
            push.entry.kind,
            StackEntryKind::Spell {
                ability: None,
                actual_mana_spent: 0,
                ..
            }
        ),
        "CR 601.2a: the recorded push is the announcement entry, got {:?}",
        push.entry.kind
    );
    assert!(
        matches!(
            landed.kind,
            StackEntryKind::Spell {
                ability: Some(_),
                ..
            }
        ),
        "CR 601.2i: the live entry was retagged with its finalized ability by a \
         seam this family does not cover, so the record above is genuinely the \
         earlier CR 601.2a snapshot rather than a copy of the live entry"
    );

    // CR 405.2: the recorded index is the pre-push depth, which is where the
    // entry ends up. Recording the post-push depth would make this 1.
    assert_eq!(
        push.resulting_position, 0,
        "CR 405.2: the push onto an empty stack occupies index 0"
    );

    // Replay-exactness: the recorded push installs the entry verbatim against
    // the captured predecessor state, with no restamp and no rescan.
    let mut replay = pre_state;
    apply_resolved_stack_push(&mut replay, push)
        .expect("the recorded push must replay against its captured predecessor");
    assert_eq!(
        replay.stack.len(),
        1,
        "replay installs exactly one stack entry"
    );
    assert_eq!(
        replay.stack[push.resulting_position], *push.entry,
        "CR 405.2: replay installs the recorded entry, verbatim, at the recorded index"
    );
    // The push family covers the push only. The Hand → Stack move is the
    // zone-change family's record, so this applier must not have moved the card.
    assert_eq!(
        replay.objects[&spell].zone,
        engine::types::zones::Zone::Hand,
        "the stack-push applier installs the entry and nothing else"
    );

    // Fail-closed: re-applying finds the stack one deeper than recorded.
    assert!(
        matches!(
            apply_resolved_stack_push(&mut replay, push),
            Err(ResolvedStackPushReplayInvariantError::StackDepthMismatch {
                expected: 0,
                found: 1
            })
        ),
        "a stack push is not idempotent: a second application must fail closed"
    );

    // Fail-closed: even at the right depth, the same entry id cannot land twice.
    let mut duplicate = push.clone();
    duplicate.resulting_position = 1;
    assert!(
        matches!(
            apply_resolved_stack_push(&mut replay, &duplicate),
            Err(ResolvedStackPushReplayInvariantError::DuplicateStackEntry(id)) if id == spell
        ),
        "the applier must refuse to duplicate a live stack entry"
    );

    // Fail-closed: the recorded controller must still be in the game.
    let mut stranger = push.clone();
    stranger.entry.controller = PlayerId(99);
    let mut fresh = runner.state().clone();
    fresh.stack.clear();
    assert!(
        matches!(
            apply_resolved_stack_push(&mut fresh, &stranger),
            Err(ResolvedStackPushReplayInvariantError::UnknownController(p)) if p == PlayerId(99)
        ),
        "the applier must refuse a controller that is not a player in this game"
    );
}

/// CR 400.7 + CR 509.1c: the journal point is AFTER the authority binds the
/// force-block source, so the record carries the bound referent itself rather
/// than the state it was derived from — and replay installs that referent
/// without going back to `state.objects` for it.
///
/// This is the test that pins the journal *placement*. Auriok Siege Sled's
/// activated ability is the reachable input shape for
/// `bind_force_block_source_recursive`; journaling above that call records
/// `force_block_attacker: None`.
#[test]
fn recorded_push_carries_the_bound_force_block_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    let sled = scenario
        .add_creature_from_oracle(P0, "Auriok Siege Sled", 3, 5, AURIOK_SIEGE_SLED)
        .id();
    let blocker = scenario.add_creature(P0, "Steel Wall", 0, 4).id();

    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, sled);
    make_artifact_creature(&mut runner, blocker);

    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();
    let sled_ref = ObjectIncarnationRef::from_object(&pre_state.objects[&sled]);

    runner
        .act(GameAction::ActivateAbility {
            source_id: sled,
            ability_index: 0,
        })
        .expect("the real force-block ability must activate");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(blocker)],
            })
            .expect("target the artifact creature that must block");
    }

    // Reach guard: the activated ability is on the stack AND it is the
    // force-block shape, so this fixture reached the binding arm rather than
    // some degenerate no-stamp path.
    let entry = runner
        .state()
        .stack
        .last()
        .expect("CR 602.2a: the activated ability is on the stack")
        .clone();
    let ability = entry
        .ability()
        .expect("an activated ability entry carries its resolved ability");
    assert_eq!(
        ability.force_block_attacker,
        Some(sled_ref),
        "reach-guard: the live entry was bound to its source, so this fixture \
         does reach bind_force_block_source_recursive"
    );

    let pushes = stack_pushes(runner.state(), journal_start);
    let push = pushes
        .iter()
        .find(|push| push.entry.id == entry.id)
        .expect("the activated ability's push must be journaled");
    assert_eq!(push.origin, ResolvedStackPushOrigin::Put);
    // An activated ability has no CR 601.2i finalization phase — its entry is
    // complete when it is pushed — so this is where field-for-field equality
    // between the record and the live entry is a meaningful assertion.
    assert_eq!(
        *push.entry, entry,
        "CR 602.2a: the record is the entry that landed, field for field"
    );

    // THE PLACEMENT ASSERTION. Journaling before the bind records `None` here.
    let recorded_ability = push
        .entry
        .ability()
        .expect("the recorded entry carries its resolved ability");
    assert_eq!(
        recorded_ability.force_block_attacker,
        Some(sled_ref),
        "CR 509.1c: the record is taken after the authority binds the source, so \
         it carries the bound referent, not an unbound ability"
    );

    // Non-rescan proof: replay into a state where the source object no longer
    // exists. A live re-derivation would produce `None`; installing the record
    // verbatim keeps the exact choice-time referent.
    let mut replay = pre_state;
    replay.objects.remove(&sled);
    apply_resolved_stack_push(&mut replay, push)
        .expect("replay must not require the source object to still exist");
    let replayed = replay.stack[push.resulting_position]
        .ability()
        .expect("the replayed entry carries its resolved ability");
    assert_eq!(
        replayed.force_block_attacker,
        Some(sled_ref),
        "CR 400.7: replay installs the recorded referent even with the source \
         gone, so it cannot be a global rescan"
    );
}

/// CR 707.10: a copy put onto the stack journals under the `Copy` origin, and
/// the originals it was copied from journal under `Put`. One command, two
/// origins — the discriminator has to actually discriminate on a real pipeline.
#[test]
fn twincast_copy_journals_a_copy_origin_push() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut tokens =
        scenario.add_spell_to_hand_from_oracle(P0, "Raise the Alarm", false, ELVISH_TOKEN_SPELL);
    tokens.with_mana_cost(ManaCost::generic(0));
    let sorcery = tokens.id();
    let mut tw = scenario.add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST);
    tw.with_mana_cost(ManaCost::generic(0));
    let twincast = tw.id();

    let mut runner = scenario.build();
    let journal_start = runner.state().resolved_rules_journal.entries().len();

    let sorcery_card = runner.state().objects[&sorcery].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: sorcery,
            card_id: sorcery_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the copyable sorcery from hand");
    let twincast_card = runner.state().objects[&twincast].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: twincast,
            card_id: twincast_card,
            targets: vec![sorcery],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Twincast targeting the sorcery");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(sorcery)],
            })
            .expect("target the sorcery with Twincast");
    }

    // CR 405.2: the two casts stacked in order onto an empty stack. Recording
    // the post-push depth instead would make these 1 and 2.
    let casts = stack_pushes(runner.state(), journal_start);
    assert_eq!(
        casts
            .iter()
            .find(|push| push.entry.id == sorcery)
            .expect("the sorcery's push is journaled")
            .resulting_position,
        0,
        "CR 405.2: the first cast occupies index 0"
    );
    assert_eq!(
        casts
            .iter()
            .find(|push| push.entry.id == twincast)
            .expect("Twincast's push is journaled")
            .resulting_position,
        1,
        "CR 405.2: Twincast goes on top of the spell it targets"
    );

    // Resolve only far enough for the copy to be created, so the live stack can
    // be compared against the copy's recorded index.
    let mut copy = None;
    for _ in 0..50 {
        if let Some(found) = stack_pushes(runner.state(), journal_start)
            .into_iter()
            .find(|push| push.origin == ResolvedStackPushOrigin::Copy)
        {
            copy = Some(found);
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("the copied sorcery has no targets to change");
            }
            other => panic!("unexpected prompt while resolving Twincast: {other:?}"),
        }
    }
    let copy = copy.expect("CR 707.10: Twincast's resolution must journal a Copy-origin push");

    assert_ne!(
        copy.entry.id, sorcery,
        "the copy is a new stack object, not the original"
    );
    assert_ne!(copy.entry.id, twincast);
    // CR 405.2 on the copy path: the recorded index is where the copy actually
    // sits on the live stack.
    assert_eq!(
        runner.state().stack[copy.resulting_position].id,
        copy.entry.id,
        "CR 405.2: the copy's recorded index is its live stack position"
    );

    // The origin discriminator separates the two authorities: exactly one Copy,
    // and both originals stayed Put.
    let all = stack_pushes(runner.state(), journal_start);
    assert_eq!(
        all.iter()
            .filter(|push| push.origin == ResolvedStackPushOrigin::Copy)
            .count(),
        1,
        "CR 707.10: exactly one copy was put onto the stack"
    );
    assert!(
        all.iter()
            .filter(|push| push.entry.id == sorcery || push.entry.id == twincast)
            .all(|push| push.origin == ResolvedStackPushOrigin::Put),
        "CR 405.1: the cast originals are Put pushes, never Copy"
    );
}
