//! CR733 P2 coverage for the CR 405.2 single-entry stack removal.
//!
//! Every one-entry removal funnels through `stack::remove_stack_entry_at`, which
//! drops the entry together with the two per-entry side tables keyed on it: the
//! CR 405.5 resolution pop and the drain loops (batched resolution, inert no-op
//! batches, CR 724.1b end-phase exile) via the `pop_top_stack_entry` wrapper,
//! the CR 701.6a counter, and the CR 601.2a / CR 601.2i cast rollbacks.
//!
//! ONE COMMAND PARAMETERIZED BY `index`, not a pop/remove-at pair. A top pop is
//! the removal at `index == resulting_depth`, so a separate pop command would
//! carry a field its caller could derive.
//! `countering_a_buried_spell_journals_a_removal_at_the_recorded_index` is what
//! makes the field non-vacuous — it asserts `index != resulting_depth`, which is
//! unreachable through any pop.
//!
//! A drain of N entries journals N separate commands rather than one bulk
//! removal, so a replay reproduces the removal ORDER and not merely the final
//! depth. `resolving_two_spells_journals_two_pops_in_lifo_order` is what pins
//! that: it would still pass on a bulk record if it only checked the end state,
//! so it asserts the per-command depths AND indices descend.
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
use engine::game::stack::{apply_resolved_stack_push, apply_resolved_stack_removal};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedStackPushCommand, ResolvedStackPushReplayInvariantError,
    ResolvedStackRemovalCommand, ResolvedStackRemovalReplayInvariantError,
};

/// A no-target sorcery: nothing to retarget, and no trigger to add stack entries
/// the pop assertions would have to filter around.
const ELVISH_TOKEN_SPELL: &str = "Create a 1/1 green Elf Warrior creature token.";

/// Every stack pop journaled after `from`, in journal order.
fn stack_removals(state: &GameState, from: usize) -> Vec<ResolvedStackRemovalCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(from)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::StackRemoval(command) => Some(*command),
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
    let pops = stack_removals(runner.state(), journal_start);
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
        pop.index, 0,
        "CR 405.2: a top-of-stack pop is the removal at index == resulting_depth, \
         which is what makes a separate pop command unnecessary"
    );
    assert_eq!(
        *pop.entry, before_pop.stack[0],
        "the recorded entry is the entry that was on the stack, verbatim"
    );

    // Replay-exactness: from the captured predecessor, applying the record
    // reproduces the removal with nothing re-derived.
    let mut replay = before_pop.clone();
    apply_resolved_stack_removal(&mut replay, pop)
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
            apply_resolved_stack_removal(&mut replay, pop),
            Err(ResolvedStackRemovalReplayInvariantError::DepthMismatch {
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
    let pops = stack_removals(runner.state(), journal_start);
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
            apply_resolved_stack_removal(&mut too_deep, pop),
            Err(ResolvedStackRemovalReplayInvariantError::DepthMismatch {
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
            apply_resolved_stack_removal(&mut wrong_top, pop),
            Err(ResolvedStackRemovalReplayInvariantError::RemovedEntryMismatch)
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

    let pops: Vec<_> = stack_removals(runner.state(), journal_start)
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
    assert_eq!(
        (pops[0].index, pops[1].index),
        (1, 0),
        "CR 405.2: each record names the index its entry occupied at the moment \
         it was removed, not its index in the original stack"
    );
}

/// CR 701.6a: a counter removes an entry that is NOT on top, which is the case
/// the recorded index exists for. A pop-shaped command could not express it.
#[test]
fn countering_a_buried_spell_journals_a_removal_at_the_recorded_index() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_spell_to_hand_from_oracle(P0, "Journal Counter Victim", true, ELVISH_TOKEN_SPELL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let counterspell = scenario
        // Instant speed: a counterspell must be castable while its target is on
        // the stack, which is the whole point of the buried-index fixture.
        .add_spell_to_hand_from_oracle(P0, "Journal Counterspell", true, "Counter target spell.")
        .with_mana_cost(ManaCost::zero())
        .id();
    // A third entry is REQUIRED for this fixture to be non-vacuous. The
    // counterspell removes itself from the stack before its effect executes
    // (CR 608.2 resolution pops first), so with only two entries the victim
    // would be on TOP when countered and the index would be indistinguishable
    // from a pop. The filler sits above the victim and stays there.
    let filler = scenario
        .add_spell_to_hand_from_oracle(P0, "Journal Counter Filler", true, ELVISH_TOKEN_SPELL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    cast(&mut runner, victim);
    cast(&mut runner, filler);
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let predecessor = runner.state().clone();
    assert_eq!(
        predecessor.stack.len(),
        2,
        "reach guard: the victim is BURIED under the filler before the counter is cast"
    );

    // The `targets` field on `CastSpell` does NOT pre-fill target selection —
    // the engine parks in `WaitingFor::TargetSelection` — so the choice goes
    // through the runner's builder, which submits it.
    runner
        .cast(counterspell)
        .target_object(victim)
        .commit()
        .resolve();
    runner.advance_until_stack_empty();

    let removals = stack_removals(runner.state(), journal_start);
    let victim_removal = removals
        .iter()
        .find(|removal| removal.entry.id == victim)
        .expect("CR 701.6a: countering the victim journals its removal");

    // The discriminating assertion for the index field: the victim sat UNDER the
    // counterspell, so it was removed from index 0 while the stack still held
    // the counterspell above it. A pop-shaped record would have described the
    // wrong entry.
    assert_eq!(
        victim_removal.index, 0,
        "CR 701.6a: the countered spell is removed from the index it occupied, \
         underneath the counterspell"
    );
    assert!(
        victim_removal.resulting_depth >= 1,
        "the counterspell is still on the stack when its target is removed, so \
         the removal is genuinely not a top-of-stack pop (depth {})",
        victim_removal.resulting_depth
    );
    assert_ne!(
        victim_removal.index, victim_removal.resulting_depth,
        "index != resulting_depth is exactly the case a pop-only command could \
         not express"
    );

    // Replay-exactness against a reconstructed predecessor: the recorded index
    // is used verbatim rather than re-scanned.
    let mut replay = predecessor.clone();
    // Rebuild the pre-removal stack: victim at index 0, counterspell above it.
    while replay.stack.len() < victim_removal.resulting_depth + 1 {
        let mut filler = replay.stack[0].clone();
        filler.id = ObjectId(9100 + replay.stack.len() as u64);
        replay.stack.push_back(filler);
    }
    apply_resolved_stack_removal(&mut replay, victim_removal)
        .expect("the recorded counter removal replays at its recorded index");
    assert!(
        !replay.stack.iter().any(|entry| entry.id == victim),
        "replay removes the countered entry"
    );
    assert_eq!(
        replay.stack.len(),
        victim_removal.resulting_depth,
        "replay lands on the recorded depth"
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

    let pop = stack_removals(runner.state(), journal_start)
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
    apply_resolved_stack_removal(&mut sequenced, &pop)
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

/// CR 800.4a: a leaving player's spells are removed from the stack. This is the
/// PLURAL case — one `retain` pass used to drop several entries at once — and it
/// is journaled as one command per entry rather than one bulk record.
///
/// The load-bearing assertion is that BOTH removals record index 0. An
/// implementation that captured each entry's ORIGINAL position would record
/// (0, 1); recording (0, 0) is what proves each index is the LIVE index at the
/// moment that entry was removed, which is the only form a sequential replay can
/// reproduce.
///
/// The leaving player is the one casting, so no priority juggling is needed: a
/// self-targeted burn spell takes its own controller to 0 and the CR 704.5a
/// state-based action runs the sweep through the real pipeline.
#[test]
fn eliminating_a_player_journals_one_removal_per_controlled_stack_entry() {
    // Three players so the elimination does not end the game outright.
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario.add_bolt_to_hand(P0);
    let victim_lower = scenario
        .add_spell_to_hand_from_oracle(P0, "Journal Victim Lower", true, ELVISH_TOKEN_SPELL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let victim_upper = scenario
        .add_spell_to_hand_from_oracle(P0, "Journal Victim Upper", true, ELVISH_TOKEN_SPELL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("the caster is in the game")
        .life = 3;

    cast(&mut runner, victim_lower);
    cast(&mut runner, victim_upper);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![victim_lower, victim_upper],
        "reach guard: both of the leaving player's spells are on the stack"
    );
    let journal_start = runner.state().resolved_rules_journal.entries().len();

    // CR 704.5a through the real pipeline: self-targeted burn takes the caster
    // to 0 and the state-based action eliminates them, running the CR 800.4a
    // sweep over the two spells still on the stack beneath it.
    runner.cast(bolt).target_player(P0).resolve();

    assert!(
        runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P0)
            .expect("the player record survives elimination")
            .is_eliminated,
        "CR 704.5a reach guard: the caster actually left the game"
    );

    let victim_removals: Vec<_> = stack_removals(runner.state(), journal_start)
        .into_iter()
        .filter(|removal| removal.entry.id == victim_lower || removal.entry.id == victim_upper)
        .collect();
    assert_eq!(
        victim_removals.len(),
        2,
        "CR 800.4a: each removed entry is journaled separately, not as one bulk record"
    );
    assert_eq!(
        (victim_removals[0].entry.id, victim_removals[1].entry.id),
        (victim_lower, victim_upper),
        "the sweep removes by ascending position, and the records carry that order"
    );
    assert_eq!(
        (victim_removals[0].index, victim_removals[1].index),
        (0, 0),
        "each index is the LIVE index at its own removal: once the lower spell \
         leaves index 0 the upper one slides down into it. Original-position \
         capture would record (0, 1) and a replay would remove the wrong entry"
    );
    assert_eq!(
        (
            victim_removals[0].resulting_depth,
            victim_removals[1].resulting_depth
        ),
        (1, 0),
        "the depths descend one per removal"
    );
    assert!(
        runner.state().stack.is_empty(),
        "CR 800.4a: every spell the leaving player controlled is gone"
    );
}
