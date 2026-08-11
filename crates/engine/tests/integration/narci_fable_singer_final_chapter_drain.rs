//! CR 714.2e + CR 714.4 + CR 608.2k — Narci, Fable Singer's final-chapter
//! drain, driven end-to-end through the production Saga pipeline.
//!
//! Oracle: `Whenever the final chapter ability of a Saga you control resolves,
//! each opponent loses X life and you gain X life, where X is that Saga's mana
//! value.`
//!
//! What makes this worth a runtime test rather than a parser assertion: the
//! chapter number and the Saga's mana value are read at two different moments
//! and the Saga does not survive between them. CR 704.5s sacrifices a Saga as
//! soon as its final chapter ability leaves the stack, so by the time Narci's
//! own trigger resolves, "that Saga" is a last-known-information reference to a
//! permanent that no longer exists. A test that only checked the AST would pass
//! while X resolved to 0.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NARCI_ORACLE: &str = "Lifelink\n\
Whenever you sacrifice an enchantment, draw a card.\n\
Whenever the final chapter ability of a Saga you control resolves, each opponent loses X life and you gain X life, where X is that Saga's mana value.";

/// A two-chapter Saga whose chapters are inert with respect to life totals, so
/// the only life change in the test is Narci's drain.
const SAGA_ORACLE: &str = "I — Create a 1/1 white Soldier creature token.\n\
II — Create a 1/1 white Soldier creature token.";

fn lore_count(runner: &GameRunner, saga_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&saga_id)
        .and_then(|obj| obj.counters.get(&CounterType::Lore).copied())
        .unwrap_or(0)
}

/// Park the game at the end of P0's turn so the next `advance_to_phase` walks
/// through P1's turn and back into a fresh P0 precombat main — the CR 714.3c
/// turn-based action that adds the Saga's next lore counter.
fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

/// Resolve everything currently on the stack, answering trigger-order prompts.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..64 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            let _ = runner.act(GameAction::PassPriority);
            let _ = runner.act(GameAction::PassPriority);
        } else {
            break;
        }
    }
}

#[test]
fn narci_drains_for_the_sagas_mana_value_when_its_final_chapter_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Narci's sacrifice-an-enchantment trigger fires when CR 704.5s sacrifices
    // the Saga; give P0 something to draw so that draw cannot end the game.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    // Both players must survive their draw steps while the Saga walks to its
    // final chapter — an empty-library draw would end the game (CR 104.3c)
    // before the chapter ability ever resolves.
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);

    scenario
        .add_creature(P0, "Narci, Fable Singer", 3, 3)
        .as_legendary()
        .from_oracle_text(NARCI_ORACLE);

    // {3} — mana value 3, the amount the drain must move.
    let saga_id = scenario
        .add_creature(P0, "Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(SAGA_ORACLE)
        .id();
    // CR 714.3a: scenario seeding bypasses the ETB pipeline, so stand in for the
    // lore counter the Saga would have entered with. The next precombat main's
    // turn-based action (CR 714.3c) then takes it to its FINAL chapter.
    scenario.with_counter(saga_id, CounterType::Lore, 1);

    let mut runner = scenario.build();

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    // CR 714.3c: the next precombat main adds the second (final) lore counter.
    park_for_next_p0_precombat_main(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);

    assert_eq!(
        lore_count(&runner, saga_id),
        2,
        "CR 714.3c must add the Saga's final lore counter"
    );

    drain_stack(&mut runner);

    // CR 704.5s: the Saga is sacrificed once its final chapter ability has left
    // the stack, so Narci's drain resolved against last-known information.
    assert_ne!(
        runner
            .state()
            .objects
            .get(&saga_id)
            .map(|saga| saga.zone)
            .unwrap_or(Zone::Graveyard),
        Zone::Battlefield,
        "CR 704.5s must sacrifice the Saga after its final chapter resolves"
    );

    assert_eq!(
        (
            runner.state().players[P0.0 as usize].life - p0_life_before,
            runner.state().players[P1.0 as usize].life - p1_life_before,
        ),
        (3, -3),
        "the final chapter ability resolving must drain each opponent for the Saga's mana value (3) and gain that much"
    );
}

/// CR 714.2e: a NON-final chapter ability resolving must not fire the trigger.
/// Without the chapter/final-chapter comparison this test drains on chapter I
/// as well, which would double Narci's output on every multi-chapter Saga.
#[test]
fn narci_does_not_drain_on_a_nonfinal_chapter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    // Both players must survive their draw steps while the Saga walks to its
    // final chapter — an empty-library draw would end the game (CR 104.3c)
    // before the chapter ability ever resolves.
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);

    scenario
        .add_creature(P0, "Narci, Fable Singer", 3, 3)
        .as_legendary()
        .from_oracle_text(NARCI_ORACLE);

    // Three chapters: the counter added below reaches chapter II, not the final
    // chapter III.
    let saga_id = scenario
        .add_creature(P0, "Long Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(
            "I — Create a 1/1 white Soldier creature token.\n\
II — Create a 1/1 white Soldier creature token.\n\
III — Create a 1/1 white Soldier creature token.",
        )
        .id();
    // CR 714.3a stand-in, as above: the advance below reaches chapter II.
    scenario.with_counter(saga_id, CounterType::Lore, 1);

    let mut runner = scenario.build();
    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    park_for_next_p0_precombat_main(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);

    assert_eq!(lore_count(&runner, saga_id), 2);
    drain_stack(&mut runner);

    // CR 704.5s: chapter II left the stack and the Saga survived, so a chapter
    // ability really did resolve here — the life assertion below is about the
    // final-chapter comparison, not about nothing having happened.
    assert_eq!(
        runner.state().objects[&saga_id].zone,
        Zone::Battlefield,
        "a three-chapter Saga is not sacrificed after chapter II"
    );
    assert_eq!(
        (
            runner.state().players[P0.0 as usize].life,
            runner.state().players[P1.0 as usize].life,
        ),
        (p0_life_before, p1_life_before),
        "chapter II of a three-chapter Saga is not the final chapter ability"
    );
}

/// CR 608.2c: a final chapter ability that PAUSES mid-resolution for a choice
/// must not fire its observers until that resolution has actually finished.
///
/// The chapter-resolution event is published next to the engine's own
/// `StackResolved`, before the settlement guard, so this pins the ordering
/// empirically rather than by assertion: the drain must not have landed while
/// the optional-effect prompt is still open, and must land exactly once after it
/// is answered.
#[test]
fn narci_does_not_drain_until_a_paused_final_chapter_finishes_resolving() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);

    scenario
        .add_creature(P0, "Narci, Fable Singer", 3, 3)
        .as_legendary()
        .from_oracle_text(NARCI_ORACLE);

    // Chapter II is the final chapter and pauses for an optional-effect choice
    // during its own resolution.
    let saga_id = scenario
        .add_creature(P0, "Paused Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(
            "I — Create a 1/1 white Soldier creature token.\n\
II — You may draw a card.",
        )
        .id();
    scenario.with_counter(saga_id, CounterType::Lore, 1);

    let mut runner = scenario.build();
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    park_for_next_p0_precombat_main(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(lore_count(&runner, saga_id), 2);

    // Walk the stack until the chapter ability's own optional choice is offered.
    let mut saw_optional = false;
    for _ in 0..64 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if let WaitingFor::OptionalEffectChoice { .. } = runner.state().waiting_for {
            saw_optional = true;
            assert_eq!(
                runner.state().players[P1.0 as usize].life,
                p1_life_before,
                "the drain must not land while the final chapter ability is still resolving"
            );
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("answer the chapter ability's optional draw");
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            let _ = runner.act(GameAction::PassPriority);
            let _ = runner.act(GameAction::PassPriority);
        } else {
            break;
        }
    }

    assert!(
        saw_optional,
        "reach guard: chapter II's optional draw must actually be offered, \
         otherwise this test never exercises a paused resolution"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life - p1_life_before,
        -3,
        "after the paused chapter ability finishes, the drain lands exactly once"
    );
}

/// CR 603.2: the other end of the lifecycle axis — Historian's Boon observes the
/// final chapter ability *triggering*, which is the Saga's own lore-counter
/// threshold crossing, not a resolution. Narci's clause is identical except for
/// that verb, so the two share one trigger mode and one matcher; this pins the
/// half Narci does not exercise.
#[test]
fn a_final_chapter_triggers_observer_fires_on_the_lore_crossing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);

    scenario
        .add_creature(P0, "Chapter Watcher", 2, 2)
        .from_oracle_text(
            "Whenever the final chapter ability of a Saga you control triggers, each opponent loses 1 life.",
        );

    let saga_id = scenario
        .add_creature(P0, "Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(SAGA_ORACLE)
        .id();
    scenario.with_counter(saga_id, CounterType::Lore, 1);

    let mut runner = scenario.build();
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    park_for_next_p0_precombat_main(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);

    assert_eq!(lore_count(&runner, saga_id), 2);
    drain_stack(&mut runner);

    assert_eq!(
        runner.state().players[P1.0 as usize].life - p1_life_before,
        -1,
        "the lore crossing onto the final chapter must fire a `triggers` observer exactly once"
    );
}
