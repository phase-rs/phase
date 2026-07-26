//! CR733 P2 coverage for the CR 405.2 top-of-stack removal.
//!
//! CR 405.5: "When all players pass in succession, the top (last-added) object
//! on the stack resolves." That removal, plus the drain loops that clear several
//! entries in one pass (batched resolution, inert no-op batches, and the
//! CR 724.1b end-phase stack exile), all funnel through the single authority
//! `stack::pop_top_stack_entry`, which drops the entry together with the two
//! per-entry side tables keyed on it.
//!
//! A drain of N entries journals N separate commands rather than one bulk
//! removal, so a replay reproduces the removal ORDER and not merely the final
//! depth. `resolving_two_spells_journals_two_pops_in_lifo_order` is what pins
//! that: it would still pass on a bulk record if it only checked the end state,
//! so it asserts the per-command depths descend.
//!
//! THE CROSS-POP CANARY IS THE ACCEPTANCE TEST FOR THIS FAMILY. Before pops were
//! journaled, `apply_resolved_stack_push` failed `StackDepthMismatch` on any
//! replay whose prefix crossed a removal, and the push suite's module header
//! recorded that every replay there deliberately used a pop-free prefix.
//! `a_recorded_pop_unblocks_a_later_push_replay` flips exactly that: from ONE
//! predecessor it shows the push alone still failing and pop-then-push
//! succeeding. Do not weaken it to a bare success assertion — the failing half
//! is what proves the pop record is doing the work.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::stack::{apply_resolved_stack_pop, apply_resolved_stack_push};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedStackPopCommand, ResolvedStackPopReplayInvariantError,
    ResolvedStackPushCommand, ResolvedStackPushReplayInvariantError,
};

/// A no-target sorcery: nothing to retarget, and no trigger to add stack entries
/// the pop assertions would have to filter around.
const ELVISH_TOKEN_SPELL: &str = "Create a 1/1 green Elf Warrior creature token.";

/// Every stack pop journaled after `from`, in journal order.
fn stack_pops(state: &GameState, from: usize) -> Vec<ResolvedStackPopCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(from)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::StackPop(command) => Some(*command),
            _ => None,
        })
        .collect()
}

/// Every stack push journaled after `from`, in journal order.
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

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the real cast must put the spell on the stack");
}

fn scenario_with_spells(names: &[&str]) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ids = names
        .iter()
        .map(|name| {
            scenario
                .add_spell_to_hand_from_oracle(P0, name, true, ELVISH_TOKEN_SPELL)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    (scenario.build(), ids)
}

#[test]
fn resolving_a_spell_journals_an_exact_pop() {
    let (mut runner, ids) = scenario_with_spells(&["Journal Pop One"]);
    let spell = ids[0];
    cast(&mut runner, spell);

    // Reach guard: the pop assertions below are meaningless unless the spell is
    // genuinely on the stack first.
    let before_pop = runner.state().clone();
    assert_eq!(
        before_pop.stack.len(),
        1,
        "CR 405.1: the cast spell is the only object on the stack"
    );
    let journal_start = before_pop.resolved_rules_journal.entries().len();

    runner.resolve_top();

    // The discriminating assertion: the removal is journaled. A raw
    // `stack.pop_back()` records nothing here.
    let pops = stack_pops(runner.state(), journal_start);
    let recorded: Vec<_> = pops.iter().filter(|pop| pop.entry.id == spell).collect();
    assert_eq!(
        recorded.len(),
        1,
        "CR 405.5: resolving the spell must journal exactly one pop for it"
    );
    let pop = recorded[0];
    assert_eq!(
        pop.resulting_depth, 0,
        "CR 405.2: the recorded depth is the depth AFTER the removal"
    );
    assert_eq!(
        *pop.entry, before_pop.stack[0],
        "the recorded entry is the entry that was on the stack, verbatim"
    );

    // Replay-exactness: from the captured predecessor, applying the record
    // reproduces the removal with nothing re-derived.
    let mut replay = before_pop.clone();
    apply_resolved_stack_pop(&mut replay, pop)
        .expect("the recorded pop must replay against its captured predecessor");
    assert!(replay.stack.is_empty(), "replay removes the recorded entry");
    assert!(
        !replay.stack_paid_facts.contains_key(&spell),
        "replay drops the paid-facts row keyed on the removed entry"
    );
    assert!(
        !replay.stack_trigger_event_batches.contains_key(&spell),
        "replay drops the trigger-batch row keyed on the removed entry"
    );

    // Re-applying is not idempotent: the stack is now shallower than recorded,
    // so it fails closed rather than popping an unrelated object.
    assert!(
        matches!(
            apply_resolved_stack_pop(&mut replay, pop),
            Err(ResolvedStackPopReplayInvariantError::DepthMismatch {
                expected: 1,
                found: 0
            })
        ),
        "a stack pop is not idempotent: a second application must fail closed"
    );
}

/// Each probe diverges exactly one axis, so a rejection can only come from the
/// precondition being probed.
#[test]
fn pop_rejects_a_divergent_predecessor() {
    let (mut runner, ids) = scenario_with_spells(&["Journal Pop Two"]);
    let spell = ids[0];
    cast(&mut runner, spell);
    let before_pop = runner.state().clone();
    let journal_start = before_pop.resolved_rules_journal.entries().len();
    runner.resolve_top();
    let pops = stack_pops(runner.state(), journal_start);
    let pop = pops
        .iter()
        .find(|pop| pop.entry.id == spell)
        .expect("the resolution journaled a pop");

    // Right entry, wrong depth: a deeper stack means the replay is not at the
    // point the record describes.
    let mut too_deep = before_pop.clone();
    let mut duplicate = before_pop.stack[0].clone();
    duplicate.id = ObjectId(9999);
    too_deep.stack.push_front(duplicate);
    assert!(
        matches!(
            apply_resolved_stack_pop(&mut too_deep, pop),
            Err(ResolvedStackPopReplayInvariantError::DepthMismatch {
                expected: 1,
                found: 2
            })
        ),
        "a pop must refuse a predecessor at the wrong depth"
    );
    assert_eq!(
        too_deep.stack.len(),
        2,
        "the rejected replay must not have mutated the stack"
    );

    // Right depth, wrong entry on top. Comparing the entry WHOLE rather than by
    // id is what catches this — an applier matching on `id` alone would happily
    // discard a divergent object that reused the identifier.
    let mut wrong_top = before_pop.clone();
    wrong_top
        .stack
        .back_mut()
        .expect("the predecessor has the entry on top")
        .source_id = ObjectId(4242);
    assert!(
        matches!(
            apply_resolved_stack_pop(&mut wrong_top, pop),
            Err(ResolvedStackPopReplayInvariantError::PoppedEntryMismatch)
        ),
        "a pop must refuse a predecessor whose top entry diverges from the record"
    );
    assert_eq!(
        wrong_top.stack.len(),
        1,
        "the rejected replay must not have mutated the stack"
    );
}

/// A drain records one command per entry, in removal order — not one bulk
/// removal that only pins the final depth.
#[test]
fn resolving_two_spells_journals_two_pops_in_lifo_order() {
    let (mut runner, ids) = scenario_with_spells(&["Journal Pop Lower", "Journal Pop Upper"]);
    let (lower, upper) = (ids[0], ids[1]);
    cast(&mut runner, lower);
    cast(&mut runner, upper);

    // Reach guard: both entries are live, and `upper` is the one CR 405.5 will
    // remove first.
    let before = runner.state().clone();
    assert_eq!(before.stack.len(), 2, "both spells are on the stack");
    assert_eq!(
        before.stack.back().map(|entry| entry.id),
        Some(upper),
        "CR 405.2: the last-added spell is on top"
    );
    let journal_start = before.resolved_rules_journal.entries().len();

    runner.advance_until_stack_empty();

    let pops: Vec<_> = stack_pops(runner.state(), journal_start)
        .into_iter()
        .filter(|pop| pop.entry.id == lower || pop.entry.id == upper)
        .collect();
    assert_eq!(pops.len(), 2, "each removal is journaled separately");
    assert_eq!(
        (pops[0].entry.id, pops[1].entry.id),
        (upper, lower),
        "CR 405.5: the last-added object resolves first, and the records carry \
         that order"
    );
    assert_eq!(
        (pops[0].resulting_depth, pops[1].resulting_depth),
        (1, 0),
        "the recorded depths descend one per removal, which a single bulk record \
         could not express"
    );
}

/// THE CANARY FLIP. See the module header: before this family, a replay whose
/// prefix crossed a pop failed `StackDepthMismatch` in the push applier.
#[test]
fn a_recorded_pop_unblocks_a_later_push_replay() {
    let (mut runner, ids) = scenario_with_spells(&["Journal Pop First", "Journal Push Second"]);
    let (first, second) = (ids[0], ids[1]);

    cast(&mut runner, first);
    let predecessor = runner.state().clone();
    assert_eq!(
        predecessor.stack.len(),
        1,
        "reach guard: the replay predecessor has the first spell on the stack"
    );
    let journal_start = predecessor.resolved_rules_journal.entries().len();

    // Resolve the first spell (journals a pop), then cast the second (journals a
    // push at the depth the pop left behind).
    runner.resolve_top();
    cast(&mut runner, second);

    let pop = stack_pops(runner.state(), journal_start)
        .into_iter()
        .find(|pop| pop.entry.id == first)
        .expect("resolving the first spell journaled its pop");
    let push = stack_pushes(runner.state(), journal_start)
        .into_iter()
        .find(|push| push.entry.id == second)
        .expect("casting the second spell journaled its push");
    assert_eq!(
        push.resulting_position, 0,
        "CR 405.2: the second spell lands at index 0 precisely because the pop \
         emptied the stack first"
    );

    // BEFORE-half: the push alone cannot replay against this predecessor,
    // because the un-removed first spell leaves the stack one deeper than the
    // push recorded. This is the exact failure the push suite's header records.
    let mut push_only = predecessor.clone();
    assert!(
        matches!(
            apply_resolved_stack_push(&mut push_only, &push),
            Err(ResolvedStackPushReplayInvariantError::StackDepthMismatch {
                expected: 0,
                found: 1
            })
        ),
        "the push record must still fail against a predecessor that has not had \
         the pop applied — otherwise this test proves nothing about the pop"
    );

    // AFTER-half: same predecessor, pop applied first, and the push now lands.
    let mut sequenced = predecessor.clone();
    apply_resolved_stack_pop(&mut sequenced, &pop)
        .expect("the recorded pop replays against the predecessor");
    apply_resolved_stack_push(&mut sequenced, &push)
        .expect("with the pop applied, the push replays across it");
    assert_eq!(
        sequenced.stack.len(),
        1,
        "the sequenced replay leaves exactly the second spell on the stack"
    );
    assert_eq!(
        sequenced.stack[push.resulting_position], *push.entry,
        "CR 405.2: the replay installs the recorded entry at the recorded index"
    );
}
