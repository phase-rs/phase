//! CR733 P2 coverage for the CR 601.2i cast finalization.
//!
//! CR 601.2a puts a spell on the stack as a STUB: `announce_spell_on_stack`
//! pushes `StackEntryKind::Spell { ability: None, actual_mana_spent: 0, .. }`
//! because neither is known until costs are chosen and paid. CR 601.2i is where
//! "the spell becomes cast" and both are written back onto that same entry,
//! together with the `stack_paid_facts` snapshot the rest of the engine reads
//! for X, kicker, convoke, and colors-spent questions.
//!
//! Both mutations are one command because they settle together. A replay that
//! installed the retagged entry without the snapshot would leave a finalized
//! spell whose paid facts are missing, which is why the applier checks the
//! snapshot precondition BEFORE installing either half.
//!
//! The fixture is a sorcery WITH a spell ability, deliberately. A vanilla
//! permanent spell has `ability: None` on both sides of the retag (see the
//! `prepared.ability_def.is_none()` branch in `casting.rs`), so it can only
//! move `actual_mana_spent` and cannot tell "the ability was written back" from
//! "the ability was never written". Do not replace it with a creature.

use engine::game::scenario::{GameScenario, P0};
use engine::game::stack::apply_resolved_stack_entry_finalize;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedStackEntryFinalizeCommand,
    ResolvedStackEntryFinalizeReplayInvariantError,
};

const DRAW_ORACLE: &str = "Draw a card.";

/// Casts a 2-generic sorcery and leaves it on the stack, unresolved.
///
/// The spell is NOT resolved: the CR 601.2i retag is what is under test, and
/// resolving would pop the entry and clear its paid facts.
fn cast_and_hold_on_stack() -> (GameState, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Journal Draw", true, DRAW_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, spell, false, Vec::new()))
            .collect(),
    );

    let mut runner = scenario.build();
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a 2-generic sorcery is castable with exactly two colorless in pool");

    (runner.state().clone(), journal_start)
}

/// Every CR 601.2i finalization journaled after `from`, in journal order.
fn finalizations(state: &GameState, from: usize) -> Vec<ResolvedStackEntryFinalizeCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(from)
        .filter_map(|entry| entry.command.as_ref())
        .filter_map(|command| match command {
            ResolvedRulesCommand::StackEntryFinalize(command) => Some(command.as_ref().clone()),
            _ => None,
        })
        .collect()
}

/// Rebuilds the pre-finalize predecessor from the post-cast state by restoring
/// exactly what the command says it replaced.
///
/// The predecessor for this family is mid-cast — after the CR 601.2a
/// announcement, before the CR 601.2i retag — so no fixture can capture it
/// directly. Reconstructing it from the recorded `expected_*` values is what
/// makes the replay assertion meaningful: if the applier installed anything the
/// command did not record, the round trip would not land back on the live state.
fn pre_finalize_state(state: &GameState, command: &ResolvedStackEntryFinalizeCommand) -> GameState {
    let mut predecessor = state.clone();
    predecessor
        .stack
        .get_mut(command.entry_position)
        .expect("the recorded position is live in the post-cast state")
        .kind = command.expected_old_kind.as_ref().clone();
    match command.expected_old_paid_facts.as_deref() {
        Some(previous) => {
            predecessor
                .stack_paid_facts
                .insert(command.object, previous.clone());
        }
        None => {
            predecessor.stack_paid_facts.remove(&command.object);
        }
    }
    predecessor
}

#[test]
fn real_cast_journals_an_exact_stack_entry_finalize() {
    let (state, journal_start) = cast_and_hold_on_stack();

    // CR 601.2i reach guard: the spell is on the stack AND finalized. Without
    // this the journal assertions below could pass on a cast that never
    // committed.
    let entry = state.stack.back().expect("the spell is on the stack");
    let (live_ability, live_mana) = match &entry.kind {
        StackEntryKind::Spell {
            ability,
            actual_mana_spent,
            ..
        } => (ability.is_some(), *actual_mana_spent),
        other => panic!("expected a spell stack entry, found {other:?}"),
    };
    assert!(
        live_ability,
        "CR 601.2i: the finalized entry carries the spell ability the stub lacked"
    );
    assert_eq!(
        live_mana, 2,
        "CR 601.2i: the finalized entry carries the mana actually spent"
    );

    // The discriminating assertion: the finalization is journaled as an exact
    // resolved command. A raw retag records nothing here.
    let commands = finalizations(&state, journal_start);
    assert_eq!(
        commands.len(),
        1,
        "the finalize authority must journal exactly one resolved command"
    );
    let command = &commands[0];
    assert_eq!(command.object, entry.id);
    assert_eq!(
        state.stack[command.entry_position].id, entry.id,
        "CR 405.2: the recorded position indexes the entry that was retagged"
    );

    // The recorded transition is stub -> finalized, which is what proves the
    // journal point sits AFTER the retag rather than before it.
    match command.expected_old_kind.as_ref() {
        StackEntryKind::Spell {
            ability,
            actual_mana_spent,
            ..
        } => {
            assert!(
                ability.is_none(),
                "CR 601.2a: the recorded predecessor is the announcement stub"
            );
            assert_eq!(*actual_mana_spent, 0);
        }
        other => panic!("expected a spell stack entry, found {other:?}"),
    }
    assert_eq!(
        command.resulting_kind.as_ref(),
        &entry.kind,
        "the recorded result is the entry that is actually live"
    );
    assert_eq!(
        command.resulting_paid_facts.actual_mana_spent, 2,
        "the recorded snapshot is the one the engine reads for paid facts"
    );
    assert_eq!(
        state.stack_paid_facts[&entry.id], *command.resulting_paid_facts,
        "the recorded snapshot is the snapshot that was installed"
    );

    // Replay-exactness: from the reconstructed predecessor, applying the record
    // lands back on the live entry and the live snapshot with nothing
    // re-derived.
    let mut replay = pre_finalize_state(&state, command);
    apply_resolved_stack_entry_finalize(&mut replay, command)
        .expect("the recorded finalization must replay against its predecessor");
    assert_eq!(
        replay.stack[command.entry_position].kind, entry.kind,
        "CR 601.2i: replay installs the recorded finalized entry"
    );
    assert_eq!(
        replay.stack_paid_facts[&entry.id], state.stack_paid_facts[&entry.id],
        "CR 601.2i: replay installs the recorded paid-facts snapshot"
    );

    // Re-applying is not idempotent: the predecessor no longer matches, so the
    // command fails closed rather than silently re-retagging.
    assert!(
        matches!(
            apply_resolved_stack_entry_finalize(&mut replay, command),
            Err(ResolvedStackEntryFinalizeReplayInvariantError::EntryKindMismatch(_))
        ),
        "a second application must fail closed on the pre-finalize precondition"
    );
}

#[test]
fn stack_entry_finalize_rejects_a_divergent_predecessor() {
    let (state, journal_start) = cast_and_hold_on_stack();
    let commands = finalizations(&state, journal_start);
    let command = &commands[0];

    // A position past the live stack depth.
    let mut past_end = command.clone();
    past_end.entry_position = state.stack.len();
    assert!(matches!(
        apply_resolved_stack_entry_finalize(&mut pre_finalize_state(&state, command), &past_end),
        Err(ResolvedStackEntryFinalizeReplayInvariantError::PositionOutOfRange { .. })
    ));

    // A live entry at the recorded position that is not the recorded entry.
    let mut predecessor = pre_finalize_state(&state, command);
    predecessor
        .stack
        .get_mut(command.entry_position)
        .expect("the recorded position is live")
        .id = engine::types::identifiers::ObjectId(9999);
    assert!(matches!(
        apply_resolved_stack_entry_finalize(&mut predecessor, command),
        Err(ResolvedStackEntryFinalizeReplayInvariantError::EntryIdentityMismatch { .. })
    ));

    // Applying against the already-finalized state: the entry is live and has
    // the right id, but it is not the pre-finalize entry the record replaces.
    assert!(matches!(
        apply_resolved_stack_entry_finalize(&mut state.clone(), command),
        Err(ResolvedStackEntryFinalizeReplayInvariantError::EntryKindMismatch(_))
    ));
}

/// The half-install guard: when the paid-facts side of the predecessor
/// disagrees, the entry retag must not have happened either.
///
/// CR 601.2i settles both mutations together, so a replay that retagged the
/// entry and only then discovered the snapshot mismatch would leave a finalized
/// entry with foreign paid facts — a state no execution ever produced. This is
/// the test that pins the applier's check-before-install ordering; moving the
/// snapshot check below the retag turns it red.
#[test]
fn a_divergent_paid_facts_predecessor_installs_neither_half() {
    let (state, journal_start) = cast_and_hold_on_stack();
    let commands = finalizations(&state, journal_start);
    let command = &commands[0];

    let mut predecessor = pre_finalize_state(&state, command);
    let stub_kind = predecessor.stack[command.entry_position].kind.clone();
    // Diverge ONLY the snapshot side, leaving the entry precondition satisfied,
    // so the rejection can only come from the paid-facts check.
    let mut foreign = command.resulting_paid_facts.as_ref().clone();
    foreign.actual_mana_spent += 7;
    predecessor
        .stack_paid_facts
        .insert(command.object, foreign.clone());

    assert!(
        matches!(
            apply_resolved_stack_entry_finalize(&mut predecessor, command),
            Err(ResolvedStackEntryFinalizeReplayInvariantError::PaidFactsMismatch(_))
        ),
        "a divergent snapshot predecessor must be rejected"
    );
    assert_eq!(
        predecessor.stack[command.entry_position].kind, stub_kind,
        "the rejected replay must not have retagged the entry"
    );
    assert_eq!(
        predecessor.stack_paid_facts[&command.object], foreign,
        "the rejected replay must not have installed its snapshot either"
    );
}
/// The `Some` side of `expected_old_paid_facts`, which no ordinary cast
/// produces.
///
/// The field is an `Option` because the authority is re-entered from the top by
/// its resume callers, so "no snapshot is present yet" is not a property the
/// record can assert without first proving no resume path re-reaches the insert.
/// That reasoning is only worth anything if the `Some` path actually works, and
/// a fresh cast always records `None` — so every other test in this file
/// exercises exactly one branch. This drives the other one.
///
/// Synthesised rather than driven through a resume path on purpose: the claim
/// under test is the APPLIER's contract (install iff the recorded predecessor
/// matches), which is a property of the command shape, not of any particular
/// caller that produces it.
#[test]
fn a_recorded_prior_snapshot_is_required_to_match_before_install() {
    let (state, journal_start) = cast_and_hold_on_stack();
    let commands = finalizations(&state, journal_start);
    let command = &commands[0];
    assert_eq!(
        command.expected_old_paid_facts, None,
        "reach guard: an ordinary cast records no prior snapshot, which is why \
         the Some path needs synthesising"
    );

    // A command that claims a prior snapshot was present.
    let mut prior = command.resulting_paid_facts.as_ref().clone();
    prior.actual_mana_spent = 1;
    let mut with_prior = command.clone();
    with_prior.expected_old_paid_facts = Some(Box::new(prior.clone()));

    // Against a predecessor that HAS that snapshot, it installs.
    let mut matching = pre_finalize_state(&state, command);
    matching
        .stack_paid_facts
        .insert(command.object, prior.clone());
    apply_resolved_stack_entry_finalize(&mut matching, &with_prior)
        .expect("a recorded prior snapshot that matches the predecessor must install");
    assert_eq!(
        matching.stack_paid_facts[&command.object], *command.resulting_paid_facts,
        "the recorded result overwrites the recorded prior snapshot"
    );
    assert_eq!(
        matching.stack[command.entry_position].kind, *command.resulting_kind,
        "the entry half installs alongside it"
    );

    // Against a predecessor that has NO snapshot, the same command fails closed.
    // This is the asymmetry the `Option` exists to express: absent and present
    // are different predecessors, not interchangeable ones.
    let mut absent = pre_finalize_state(&state, command);
    let stub_kind = absent.stack[command.entry_position].kind.clone();
    assert!(
        matches!(
            apply_resolved_stack_entry_finalize(&mut absent, &with_prior),
            Err(ResolvedStackEntryFinalizeReplayInvariantError::PaidFactsMismatch(_))
        ),
        "a command recording a prior snapshot must not apply where none exists"
    );
    assert_eq!(
        absent.stack[command.entry_position].kind, stub_kind,
        "and the rejected replay installs neither half"
    );
}
