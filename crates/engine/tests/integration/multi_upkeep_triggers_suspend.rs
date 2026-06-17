//! Runtime regression for issue #1502 — multiple suspended cards owned by the
//! same player must each lose a time counter at the beginning of that player's
//! upkeep. Only the first suspended card's upkeep trigger was firing; subsequent
//! suspended cards retained all of their time counters indefinitely.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.2: a game event triggers every triggered ability whose trigger
//!     event it matches — once per source instance.
//!   - CR 603.3: each triggered ability is put on the stack the next time a
//!     player would receive priority; one ability per triggering instance.
//!   - CR 702.62a: "At the beginning of your upkeep, if this card is suspended,
//!     remove a time counter from it." Synthesized for every Suspend keyword.
//!
//! The trigger is synthesized per-source by
//! `KeywordTriggerInstaller::triggers_for(&Keyword::Suspend{..})` and the exile
//! scan in `collect_pending_triggers` iterates every object in `state.exile`,
//! so the per-source firing contract must hold end-to-end: with two suspended
//! cards in exile, both must decrement by exactly one each upkeep.
//!
//! The test is hand-built (no `CardDatabase`) so it remains discriminating even
//! when the exported `card-data.json` schema drifts ahead of the engine on a
//! work-in-progress branch — `CardDatabase::from_export` returns an error in
//! that window, which would otherwise mask the bug under an early-return.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerCondition,
    TriggerConstraint, TriggerDefinition,
};
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Build the printed "At the beginning of your upkeep, if this card is
/// suspended, remove a time counter from it." trigger. Mirrors
/// `database::synthesis::build_suspend_upkeep_removal_trigger` (which is
/// crate-private) so the test pins the actual per-source firing contract
/// without depending on `CardDatabase` parsing real card-data.json.
fn build_suspend_upkeep_removal_trigger() -> TriggerDefinition {
    let remove_one = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RemoveCounter {
            counter_type: Some(CounterType::Time),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    );
    let mut trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .valid_card(TargetFilter::SelfRef)
        .condition(TriggerCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Time),
            minimum: 1,
            maximum: None,
        })
        .constraint(TriggerConstraint::OnlyDuringYourTurn)
        .execute(remove_one)
        .description(
            "CR 702.62a: At the beginning of your upkeep, if this card is suspended, \
             remove a time counter from it."
                .to_string(),
        );
    trigger.trigger_zones = vec![Zone::Exile];
    trigger
}

/// Place a suspended card (in exile) with Suspend printed, the upkeep
/// counter-removal trigger installed, and `n` time counters on it. Operates
/// directly on `state` so it works post-`scenario.build()` (the `GameScenario`
/// has no public state accessor, but `GameRunner::state_mut` is public).
fn add_suspended_card(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    time_counters: u32,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Exile);

    let suspend_kw = Keyword::Suspend {
        count: time_counters,
        cost: ManaCost::default(),
    };
    let trigger = build_suspend_upkeep_removal_trigger();

    let obj = state.objects.get_mut(&id).unwrap();
    obj.keywords.push(suspend_kw.clone());
    obj.base_keywords.push(suspend_kw);
    obj.trigger_definitions.push(trigger.clone());
    Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
    obj.counters.insert(CounterType::Time, time_counters);

    id
}

/// CR 603.2 + CR 702.62a — two suspended cards must each decrement by one
/// time counter at the beginning of their controller's upkeep. With the bug
/// only the first card's trigger fires; the second card stays at its
/// starting count. With the per-source firing contract intact both
/// decrement.
#[test]
fn multiple_suspended_cards_each_remove_time_counter() {
    let mut scenario = GameScenario::new();
    // Stock library so the Draw step doesn't deck out.
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);

    let mut runner: GameRunner = scenario.build();
    let state = runner.state_mut();

    // Two distinct suspended cards owned by P0 in the exile zone. Different
    // names ensure no accidental identity-collapse hides a per-source bug.
    let first = add_suspended_card(state, P0, "Suspended Card A", 4);
    let second = add_suspended_card(state, P0, "Suspended Card B", 4);

    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    // Preconditions.
    let counters_of = |state: &GameState, id: ObjectId| {
        state
            .objects
            .get(&id)
            .and_then(|o| o.counters.get(&CounterType::Time).copied())
            .unwrap_or(0)
    };
    assert_eq!(
        counters_of(runner.state(), first),
        4,
        "precondition: first suspended card starts with 4 time counters"
    );
    assert_eq!(
        counters_of(runner.state(), second),
        4,
        "precondition: second suspended card starts with 4 time counters"
    );

    // Drive Untap → Upkeep → Draw → PreCombatMain. Both Suspend upkeep
    // triggers must resolve during the priority drain.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    let first_after = counters_of(runner.state(), first);
    let second_after = counters_of(runner.state(), second);

    // CR 603.2 + CR 702.62a: each suspended card's own upkeep trigger fires
    // once and decrements its time counter by one. Both must read 3.
    assert_eq!(
        first_after, 3,
        "first suspended card must lose one time counter at upkeep (4 → 3), got {}",
        first_after
    );
    assert_eq!(
        second_after, 3,
        "second suspended card must also lose one time counter at upkeep (4 → 3), \
         got {}. Issue #1502: only the first suspended card decrements; \
         additional suspended cards' upkeep triggers do not fire.",
        second_after
    );
}
