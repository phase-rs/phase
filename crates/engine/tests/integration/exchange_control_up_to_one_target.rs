//! Paired-subject "exchange control of <context-ref> and up to one target X"
//! (Gilded Drake): the declared slot may be left empty.
//!
//! CR 115.6: a spell or ability that requires targets may allow zero targets to
//! be chosen. CR 603.3d: a triggered ability is only removed from the stack when
//! a required choice cannot be made, and "up to one" always has a legal answer
//! (zero targets). CR 701.12a + CR 701.12b: with no second permanent there is no
//! exchange, so "If you don't or can't make an exchange, sacrifice this
//! creature" applies.
//!
//! CR 101.1 + CR 608.2b: the trailing "This ability still resolves if its
//! target becomes illegal" overrides CR 608.2b's "doesn't resolve". When the
//! sole target is illegal at resolution the ability resolves anyway; the
//! illegal target is unaffected, so no exchange happens and the rider
//! sacrifices the Drake.
//!
//! Rows:
//! - T1, positive control: a legal target that stays legal is exchanged and the
//!   Drake is not sacrificed.
//! - T2: the sole target is bounced in response, so the ability still resolves
//!   without an exchange and the Drake is sacrificed.
//! - T3: the sole target gains shroud in response, with the same outcome; the
//!   target stays where it is.
//! - T4: zero targets chosen while a legal opponent creature exists, so the
//!   Drake is sacrificed.
//! - T5: no opponent creature at all, so the trigger still goes on the stack and
//!   the Drake is sacrificed.
//! - T6, hostile sibling: Volatile Stormdrake has no override, so the same
//!   bounce makes its trigger not resolve at all.
//! - OB1: with no opponent creature, the trigger-payoff preflight credits the
//!   Drake's trigger as fireable.

use engine::game::ability_utils::{
    ability_definition_supported, build_resolved_from_def,
    simple_legal_target_assignment_exists_for_ability, validate_targets_in_chain,
};
use engine::game::keywords::has_keyword;
use engine::game::scenario::{CastCommit, CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::triggers::hypothetical_trigger_fireable;
use engine::types::ability::{EffectKind, IllegalTargetsDisposition, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim from Scryfall (`cards/named?exact=Gilded%20Drake`).
const GILDED_DRAKE_TEXT: &str = "Flying\nWhen this creature enters, exchange control of this \
    creature and up to one target creature an opponent controls. If you don't or can't make an \
    exchange, sacrifice this creature. This ability still resolves if its target becomes illegal.";

/// Verbatim from Scryfall (`cards/named?exact=Volatile%20Stormdrake`): the same
/// exchange with a mandatory slot and no override sentence.
const VOLATILE_STORMDRAKE_TEXT: &str = "Flying, hexproof from activated and triggered \
    abilities\nWhen this creature enters, exchange control of this creature and target creature \
    an opponent controls. If you do, you get {E}{E}{E}{E}, then sacrifice that creature unless \
    you pay an amount of {E} equal to its mana value.";

/// Verbatim Oracle text of Unsummon (MTGJSON).
const UNSUMMON_TEXT: &str = "Return target creature to its owner's hand.";

/// A test spell, not a printed card: the one sentence T3 needs (CR 702.18a).
const SHROUD_TEST_SPELL_TEXT: &str = "Target creature gains shroud until end of turn.";

/// Upper bound on staging steps. The Drake needs a handful of actions (pass to
/// resolve the spell, answer the trigger prompt); the bound only exists so a
/// broken pipeline fails with a message instead of spinning forever.
const STAGING_STEP_LIMIT: usize = 40;

/// What the staging helper saw on the way to a staged trigger.
#[derive(Debug, Default)]
struct StagingObservation {
    /// The slots of every `TriggerTargetSelection` prompt answered, in order.
    prompts: Vec<Vec<TargetSelectionSlot>>,
}

/// A board with Gilded Drake in P0's hand (free to cast) and, when requested,
/// a Grizzly Bears controlled by P1. P0 is active and holds priority.
fn build_board(with_opponent_creature: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear =
        with_opponent_creature.then(|| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id());
    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Gilded Drake", 3, 3, GILDED_DRAKE_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    (runner, drake, bear)
}

/// Cast Gilded Drake and drive the real pipeline until its ETB trigger sits on
/// the stack, unresolved, with priority open.
///
/// Every `TriggerTargetSelection` prompt is answered with `answer` (`None`
/// declines the optional slot). The trigger counts as staged only once it is a
/// real stack entry: a pending trigger (`pending_trigger_entry`) still waiting
/// on its target prompt is not staged, because handing priority away at that
/// point would discard the prompt.
fn stage_drake_trigger(
    runner: &mut GameRunner,
    drake: ObjectId,
    answer: Option<ObjectId>,
) -> (CastCommit<'_>, StagingObservation) {
    let mut commit = runner.cast(drake).commit();
    let mut observation = StagingObservation::default();

    for _ in 0..STAGING_STEP_LIMIT {
        let state = commit.state();
        match &state.waiting_for {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                observation.prompts.push(target_slots.clone());
                commit
                    .act(GameAction::ChooseTarget {
                        target: answer.map(TargetRef::Object),
                    })
                    .expect("ChooseTarget should be accepted for the Drake's trigger");
            }
            WaitingFor::Priority { .. } => {
                let drake_on_battlefield =
                    state.objects.get(&drake).map(|object| object.zone) == Some(Zone::Battlefield);
                if drake_on_battlefield
                    && state.pending_trigger_entry.is_none()
                    && drake_trigger_entry(commit.state(), drake).is_some()
                {
                    return (commit, observation);
                }
                assert!(
                    !state.stack.is_empty(),
                    "the stack emptied without the Drake's ETB trigger ever reaching it \
                     (observation so far: {observation:?}, waiting_for: {:?})",
                    state.waiting_for
                );
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while staging the trigger");
            }
            other => panic!(
                "unexpected waiting state while staging the Drake's trigger: {other:?} \
                 (observation so far: {observation:?})"
            ),
        }
    }
    panic!(
        "the Drake's ETB trigger was not staged within {STAGING_STEP_LIMIT} steps \
         (observation so far: {observation:?}, waiting_for: {:?})",
        commit.state().waiting_for
    );
}

/// The declared targets of the Drake's ETB trigger on the stack, if it is there.
fn drake_trigger_entry(
    state: &engine::types::game_state::GameState,
    drake: ObjectId,
) -> Option<Vec<TargetRef>> {
    trigger_ability(state, drake).map(|ability| ability.targets.clone())
}

/// The resolved ability of `source`'s triggered ability on the stack, if any.
fn trigger_ability(
    state: &engine::types::game_state::GameState,
    source: ObjectId,
) -> Option<&ResolvedAbility> {
    state.stack.iter().find_map(|entry| {
        let is_source_trigger = entry.source_id == source
            && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. });
        if !is_source_trigger {
            return None;
        }
        entry.ability()
    })
}

/// A board with `name` (free to cast, `text` verbatim) in P0's hand, a Grizzly
/// Bears controlled by P1, and a free instant with `response_text` in P1's hand.
/// P0 is active and holds priority.
fn build_response_board(
    name: &str,
    text: &str,
    response_name: &str,
    response_text: &str,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, name, 3, 3, text)
        .with_mana_cost(ManaCost::zero())
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P1, response_name, true, response_text)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    (runner, source, bear, response)
}

/// With `source`'s ETB trigger staged on the stack, hand priority to P1, cast
/// `response` at `bear`, and pass priority until the response has resolved and
/// only the trigger is left on the stack, still unresolved.
///
/// The returned commit's `resolve()` then resolves the trigger, so its outcome
/// events are the trigger's resolution events.
fn respond_and_settle<'commit>(
    commit: &'commit mut CastCommit<'_>,
    source: ObjectId,
    response: ObjectId,
    bear: ObjectId,
) -> CastCommit<'commit> {
    // Priority goes to P1 only once the trigger is a real stack entry, the same
    // hand-off the V5 row in `exchange_control_of_a_spell.rs` makes.
    {
        let state = commit.state_mut();
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut response_commit = commit.cast(response).target_objects(&[bear]).commit();
    for _ in 0..STAGING_STEP_LIMIT {
        let state = response_commit.state();
        if state.stack.len() == 1 && trigger_ability(state, source).is_some() {
            return response_commit;
        }
        response_commit
            .act(GameAction::PassPriority)
            .expect("PassPriority should succeed while the response resolves");
    }
    panic!(
        "the response did not resolve ahead of the trigger within {STAGING_STEP_LIMIT} steps \
         (stack: {:?}, waiting_for: {:?})",
        response_commit.state().stack,
        response_commit.state().waiting_for
    );
}

fn exchange_resolutions(outcome: &CastOutcome, source: ObjectId) -> usize {
    outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ExchangeControl,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

fn sacrificed_permanents(outcome: &CastOutcome) -> Vec<ObjectId> {
    outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

fn controller_changes(outcome: &CastOutcome) -> usize {
    outcome
        .events()
        .iter()
        .filter(|event| matches!(event, GameEvent::ControllerChanged { .. }))
        .count()
}

fn drake_was_sacrificed(outcome: &CastOutcome, drake: ObjectId) -> bool {
    outcome.events().iter().any(|event| {
        matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == drake
        )
    })
}

/// Assert the trigger raised exactly one prompt whose single slot is optional
/// and offers `bear`.
///
/// This is the reach guard that separates "up to one" from a mandatory slot:
/// a mandatory slot with exactly one legal choice is bound without a prompt.
fn assert_single_optional_prompt_offers(observation: &StagingObservation, bear: ObjectId) {
    let [slots] = observation.prompts.as_slice() else {
        panic!(
            "REACH GUARD: the Drake's trigger must raise exactly one target prompt \
             (observation: {observation:?})"
        );
    };
    let [slot] = slots.as_slice() else {
        panic!("REACH GUARD: the prompt must have exactly one slot (slots: {slots:?})");
    };
    assert!(
        slot.optional,
        "REACH GUARD: CR 115.6 \"up to one target\" makes the slot optional (slot: {slot:?})"
    );
    assert!(
        slot.legal_targets.contains(&TargetRef::Object(bear)),
        "REACH GUARD: the opponent's creature must be a legal choice (slot: {slot:?})"
    );
}

/// T1, positive control. CR 701.12b: the bear is chosen and stays legal, the
/// two permanents have different controllers, so their controllers swap. The
/// exchange happened, so "If you don't or can't make an exchange" is false and
/// the Drake is not sacrificed.
#[test]
fn gilded_drake_exchanges_with_a_chosen_target_and_is_not_sacrificed() {
    let (mut runner, drake, bear) = build_board(true);
    let bear = bear.expect("the board has an opponent creature");

    let (commit, observation) = stage_drake_trigger(&mut runner, drake, Some(bear));
    assert_single_optional_prompt_offers(&observation, bear);
    assert_eq!(
        drake_trigger_entry(commit.state(), drake),
        Some(vec![TargetRef::Object(bear)]),
        "REACH GUARD: the staged trigger carries the chosen bear as its declared target"
    );

    let outcome = commit.resolve();

    assert_eq!(
        controller_changes(&outcome),
        2,
        "REACH GUARD: CR 701.12b swaps both controllers (events were {:?})",
        outcome.events()
    );
    let drake_object = outcome.state().objects.get(&drake).unwrap();
    assert_eq!(
        drake_object.zone,
        Zone::Battlefield,
        "the Drake stays on the battlefield"
    );
    assert_eq!(
        drake_object.controller, P1,
        "the Drake goes to the opponent"
    );
    assert_eq!(
        outcome.state().objects.get(&bear).unwrap().controller,
        P0,
        "and the opponent's creature comes to the Drake's controller"
    );
    assert!(
        !drake_was_sacrificed(&outcome, drake),
        "CR 608.2c: an exchange that happened must not sacrifice the Drake (events were {:?})",
        outcome.events()
    );
}

/// T4. CR 115.6: the controller may choose zero targets for "up to one target".
/// With no second permanent, CR 701.12a says no exchange happens, so the rider
/// sacrifices the Drake while the declined bear stays with its controller.
///
/// Fails on revert: a mandatory slot with exactly one legal creature is bound
/// without a prompt, so the prompt reach guard fails before any outcome check.
#[test]
fn gilded_drake_choosing_no_target_sacrifices_the_drake() {
    let (mut runner, drake, bear) = build_board(true);
    let bear = bear.expect("the board has an opponent creature");

    let (commit, observation) = stage_drake_trigger(&mut runner, drake, None);
    assert_single_optional_prompt_offers(&observation, bear);
    assert_eq!(
        drake_trigger_entry(commit.state(), drake),
        Some(Vec::new()),
        "REACH GUARD: the declined trigger is on the stack with no declared target"
    );

    let outcome = commit.resolve();

    assert!(
        drake_was_sacrificed(&outcome, drake),
        "CR 701.21a: with no exchange made, the Drake's controller sacrifices it \
         (events were {:?})",
        outcome.events()
    );
    let drake_object = outcome.state().objects.get(&drake).unwrap();
    assert_eq!(
        drake_object.zone,
        Zone::Graveyard,
        "the Drake is in a graveyard"
    );
    assert!(
        outcome.state().players[0].graveyard.contains(&drake),
        "the Drake is in its owner P0's graveyard"
    );
    let bear_object = outcome.state().objects.get(&bear).unwrap();
    assert_eq!(
        bear_object.zone,
        Zone::Battlefield,
        "the declined bear is untouched"
    );
    assert_eq!(
        bear_object.controller, P1,
        "the declined bear stays the opponent's"
    );
    assert_eq!(
        controller_changes(&outcome),
        0,
        "no control exchange happens when no target was chosen (events were {:?})",
        outcome.events()
    );
}

/// T5. CR 603.3d: with no opponent creature, "up to one target" still has a
/// legal choice (zero targets), so the trigger goes on the stack rather than
/// being removed. It resolves without an exchange and sacrifices the Drake.
///
/// Fails on revert: the mandatory slot has no legal target, the trigger never
/// reaches the stack, and the staging helper panics with the stack empty.
#[test]
fn gilded_drake_without_an_opponent_creature_still_triggers_and_is_sacrificed() {
    let (mut runner, drake, _) = build_board(false);

    let (commit, _observation) = stage_drake_trigger(&mut runner, drake, None);
    assert_eq!(
        drake_trigger_entry(commit.state(), drake),
        Some(Vec::new()),
        "REACH GUARD: the trigger is on the stack with no declared target"
    );

    let outcome = commit.resolve();

    assert!(
        drake_was_sacrificed(&outcome, drake),
        "CR 701.21a: with no exchange possible, the Drake's controller sacrifices it \
         (events were {:?})",
        outcome.events()
    );
    let drake_object = outcome.state().objects.get(&drake).unwrap();
    assert_eq!(
        drake_object.zone,
        Zone::Graveyard,
        "the Drake is in a graveyard"
    );
    assert!(
        outcome.state().players[0].graveyard.contains(&drake),
        "the Drake is in its owner P0's graveyard"
    );
}

/// T2. The bear is bounced in response, so as the trigger resolves its sole
/// target is a new object (CR 400.7) and illegal. CR 101.1 + CR 608.2b: the
/// card's override makes the ability resolve anyway; the illegal target is
/// unaffected, so CR 701.12a makes no exchange, and "If you don't or can't make
/// an exchange" sacrifices the Drake (CR 701.21a).
///
/// Fails on revert of the `resolve_top` gate or the parser stamp: the ability
/// does not resolve and the Drake stays on the battlefield under P0.
#[test]
fn gilded_drake_still_resolves_when_its_target_is_bounced_and_is_sacrificed() {
    let (mut runner, drake, bear, unsummon) =
        build_response_board("Gilded Drake", GILDED_DRAKE_TEXT, "Unsummon", UNSUMMON_TEXT);
    let (mut commit, observation) = stage_drake_trigger(&mut runner, drake, Some(bear));
    assert_single_optional_prompt_offers(&observation, bear);
    let settled = respond_and_settle(&mut commit, drake, unsummon, bear);

    let state = settled.state();
    assert_eq!(
        state.objects.get(&bear).map(|object| object.zone),
        Some(Zone::Hand),
        "REACH GUARD: the bear was bounced before the trigger resolves"
    );
    let entry = trigger_ability(state, drake).expect("the Drake's trigger is still on the stack");
    assert_eq!(
        entry.targets,
        vec![TargetRef::Object(bear)],
        "REACH GUARD: the trigger still declares the bear"
    );
    assert_eq!(
        entry.illegal_targets_disposition,
        IllegalTargetsDisposition::StillResolves,
        "REACH GUARD: the parsed override reached the stack entry"
    );

    let outcome = settled.resolve();

    assert_eq!(
        sacrificed_permanents(&outcome),
        vec![drake],
        "CR 701.21a: with no exchange made, the Drake is sacrificed (events were {:?})",
        outcome.events()
    );
    assert!(
        outcome.state().players[0].graveyard.contains(&drake),
        "the Drake is in its owner P0's graveyard"
    );
    assert_eq!(
        exchange_resolutions(&outcome, drake),
        1,
        "CR 608.2b: the ability resolved, its exchange running with no legal target \
         (events were {:?})",
        outcome.events()
    );
    assert_eq!(
        controller_changes(&outcome),
        0,
        "CR 701.12a: no exchange happens with an illegal target (events were {:?})",
        outcome.events()
    );
}

/// T3. The bear gains shroud in response, so it can't be the target of the
/// Drake's ability (CR 702.18a) and the sole target is illegal while the bear
/// stays on the battlefield. CR 101.1 + CR 608.2b: the ability still resolves,
/// the bear is unaffected (no exchange, CR 701.12a), and the Drake is
/// sacrificed (CR 701.21a).
///
/// Fails on revert of the `resolve_top` gate or the parser stamp: the ability
/// does not resolve and the Drake stays on the battlefield under P0.
#[test]
fn gilded_drake_still_resolves_when_its_target_gains_shroud_and_is_sacrificed() {
    let (mut runner, drake, bear, shroud_spell) = build_response_board(
        "Gilded Drake",
        GILDED_DRAKE_TEXT,
        "Shroud Test Spell",
        SHROUD_TEST_SPELL_TEXT,
    );
    let (mut commit, observation) = stage_drake_trigger(&mut runner, drake, Some(bear));
    assert_single_optional_prompt_offers(&observation, bear);
    let settled = respond_and_settle(&mut commit, drake, shroud_spell, bear);

    let state = settled.state();
    let bear_object = state.objects.get(&bear).unwrap();
    assert!(
        bear_object.zone == Zone::Battlefield && has_keyword(bear_object, &Keyword::Shroud),
        "REACH GUARD: the bear is still on the battlefield, now with shroud"
    );
    let entry = trigger_ability(state, drake).expect("the Drake's trigger is still on the stack");
    assert_eq!(
        entry.targets,
        vec![TargetRef::Object(bear)],
        "REACH GUARD: the trigger still declares the bear"
    );
    assert!(
        validate_targets_in_chain(state, entry).targets.is_empty(),
        "REACH GUARD: CR 608.2b re-validation finds the shrouded bear illegal"
    );

    let outcome = settled.resolve();

    assert_eq!(
        sacrificed_permanents(&outcome),
        vec![drake],
        "CR 701.21a: only the Drake is sacrificed, never the pruned bear (events were {:?})",
        outcome.events()
    );
    assert!(
        outcome.state().players[0].graveyard.contains(&drake),
        "the Drake is in its owner P0's graveyard"
    );
    let bear_object = outcome.state().objects.get(&bear).unwrap();
    assert_eq!(
        (bear_object.zone, bear_object.controller),
        (Zone::Battlefield, P1),
        "CR 608.2b: the illegal target is unaffected"
    );
}

/// T6, hostile sibling. Volatile Stormdrake prints the same exchange without
/// an override, so when its sole target is bounced in response CR 608.2b
/// applies: the ability doesn't resolve. No exchange, no energy, no sacrifice.
///
/// Fails if the gate goes soft for every ability: the dry exchange would then
/// resolve and emit its `EffectResolved` event. Paired positive: T2's one.
#[test]
fn volatile_stormdrake_without_the_override_does_not_resolve_when_its_target_is_bounced() {
    let (mut runner, stormdrake, bear, unsummon) = build_response_board(
        "Volatile Stormdrake",
        VOLATILE_STORMDRAKE_TEXT,
        "Unsummon",
        UNSUMMON_TEXT,
    );
    let (mut commit, observation) = stage_drake_trigger(&mut runner, stormdrake, Some(bear));
    assert!(
        observation.prompts.is_empty(),
        "REACH GUARD: the mandatory slot with one legal creature binds without a prompt \
         (observation: {observation:?})"
    );
    let settled = respond_and_settle(&mut commit, stormdrake, unsummon, bear);

    let state = settled.state();
    assert_eq!(
        state.objects.get(&bear).map(|object| object.zone),
        Some(Zone::Hand),
        "REACH GUARD: the bear was bounced before the trigger resolves"
    );
    let entry =
        trigger_ability(state, stormdrake).expect("the Stormdrake's trigger is still on the stack");
    assert_eq!(
        entry.targets,
        vec![TargetRef::Object(bear)],
        "REACH GUARD: the trigger still declares the bear"
    );
    assert_eq!(
        entry.illegal_targets_disposition,
        IllegalTargetsDisposition::DoesNotResolve,
        "REACH GUARD: the Stormdrake carries the CR 608.2b default"
    );
    let energy_before = state.players[0].energy;

    let outcome = settled.resolve();

    assert_eq!(
        exchange_resolutions(&outcome, stormdrake),
        0,
        "CR 608.2b: the ability does not resolve, so its exchange never runs (events were {:?})",
        outcome.events()
    );
    let stormdrake_object = outcome.state().objects.get(&stormdrake).unwrap();
    assert_eq!(
        (stormdrake_object.zone, stormdrake_object.controller),
        (Zone::Battlefield, P0),
        "the Stormdrake stays P0's on the battlefield"
    );
    assert_eq!(
        controller_changes(&outcome),
        0,
        "no control exchange happens"
    );
    assert_eq!(
        outcome.state().players[0].energy,
        energy_before,
        "no energy is gained"
    );
}

/// OB1. With no opponent creature, "up to one target" is still satisfiable by
/// choosing zero targets (CR 115.6), so the trigger is live (CR 603.3d does not
/// remove it). The trigger-payoff preflight must credit it: the paired slot's
/// spec is optional. Hostile sibling: Volatile Stormdrake's mandatory slot has
/// no legal target, so its trigger is not fireable on the same board.
#[test]
fn gilded_drake_trigger_is_fireable_in_the_preflight_without_an_opponent_creature() {
    for (name, text, expected_assignment, expected_fireable) in [
        ("Gilded Drake", GILDED_DRAKE_TEXT, Some(true), true),
        (
            "Volatile Stormdrake",
            VOLATILE_STORMDRAKE_TEXT,
            Some(false),
            false,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature_from_oracle(P0, name, 3, 3, text).id();
        let runner = scenario.build();
        let state = runner.state();
        let object = state.objects.get(&source).unwrap();
        let triggers: Vec<_> = object.trigger_definitions.iter_unchecked().collect();
        let [entry] = triggers.as_slice() else {
            panic!("{name} has exactly one trigger, got {triggers:?}");
        };
        let execute = entry
            .definition
            .execute
            .as_deref()
            .expect("the trigger has an execute");
        assert!(
            ability_definition_supported(execute),
            "REACH GUARD: {name}'s execute is supported, so the preflight reaches its slots"
        );

        let resolved = build_resolved_from_def(execute, source, P0);
        assert_eq!(
            simple_legal_target_assignment_exists_for_ability(state, &resolved, &[]),
            expected_assignment,
            "{name}: a legal target assignment exists only for an optional slot"
        );
        assert_eq!(
            hypothetical_trigger_fireable(state, object, entry),
            expected_fireable,
            "{name}: fireable only when its slot can be left empty"
        );
    }
}
