//! Daretti, Scrap Savant emblem — simultaneous artifact death produces N
//! independent delayed-return triggers (issue #3075).
//!
//! Daretti's emblem text:
//!   "Whenever an artifact is put into your graveyard from the battlefield,
//!    return that card to the battlefield at the beginning of the next end step."
//!
//! The bug: when Blasphemous Act (or any board wipe) kills N artifact creatures
//! simultaneously, the emblem trigger should fire N independent times — once per
//! ZoneChanged event — and create N independent AtNextPhase{End} delayed
//! triggers, each snapshotting a distinct artifact. At the beginning of the next
//! end step, all N delayed triggers fire and return all N artifacts to the
//! battlefield.
//!
//! The emblem is not a permanent and functions from the command zone (CR 114.4).
//! Its triggers must be scanned via the command-zone path in
//! `collect_pending_triggers`, which resets `registered_this_event` between
//! events (CR 603.2c) and uses `batched_this_pass` for "one or more" guards.
//! Daretti's trigger is NOT batched, so each ZoneChanged event fires it
//! independently.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 114.1 + CR 114.4: Emblems function from the command zone; their
//!     triggered abilities are active outside the battlefield.
//!   - CR 603.2c: "An ability triggers only once each time its trigger event
//!     occurs. However, it can trigger repeatedly if one event contains multiple
//!     occurrences." N simultaneous artifact deaths are N distinct ZoneChanged
//!     events, so the emblem trigger fires N times.
//!   - CR 603.3: Triggered abilities go on the stack the next time a player
//!     would receive priority — N trigger instances go on the stack.
//!   - CR 603.7 + CR 603.7b: Delayed triggers created by resolving the emblem
//!     trigger fire "at the beginning of the next end step" (AtNextPhase{End})
//!     and are consumed (one-shot) after firing.
//!   - CR 704.5g + CR 704.7: Simultaneous SBA destruction — all creatures with
//!     lethal damage are destroyed in one batch.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, Effect, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Drive any auto-ordering / priority passes until the stack drains.
fn drain_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// Daretti's ultimate grants this emblem via a sorcery-speed loyalty ability.
/// Use the same Oracle phrasing that the MTGJSON pipeline parses — the parser
/// stamps `trigger_zones = [Zone::Command]` on triggers inside "You get an
/// emblem with \"...\"" text (see `oracle_effect/mod.rs` CR 114.4 comment).
const DARETTI_EMBLEM_ORACLE: &str = concat!(
    "You get an emblem with \"Whenever an artifact is put into your graveyard ",
    "from the battlefield, return that card to the battlefield at the beginning ",
    "of the next end step.\""
);

/// Extract the `Effect::CreateEmblem` from a spell that was parsed from Oracle
/// text using the production parser. The spell is added to hand via
/// `add_spell_to_hand_from_oracle`; after building the scenario the first
/// `Spell` ability on the object must carry `Effect::CreateEmblem`.
fn extract_create_emblem_effect(
    state: &engine::types::game_state::GameState,
    spell_id: ObjectId,
) -> Effect {
    let obj = &state.objects[&spell_id];
    obj.abilities
        .iter()
        .find(|a| matches!(a.kind, AbilityKind::Spell))
        .and_then(|a| {
            if matches!(*a.effect, Effect::CreateEmblem { .. }) {
                Some((*a.effect).clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            panic!(
                "spell must carry a CreateEmblem Spell ability; abilities: {:#?}",
                obj.abilities
                    .iter()
                    .map(|a| (&a.kind, &*a.effect))
                    .collect::<Vec<_>>()
            )
        })
}

/// CR 114.4 + CR 603.2c regression (issue #3075): Two artifacts dying
/// simultaneously under Daretti's emblem produces two independent
/// AtNextPhase{End} delayed return triggers — one per death — and both
/// artifacts return at the next end step.
#[test]
fn daretti_emblem_simultaneous_artifact_death_produces_n_returns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A sorcery carrying the emblem-granting Oracle text. The parser converts
    // "You get an emblem with \"...\"" into `Effect::CreateEmblem { triggers }`,
    // and stamps `trigger_zones = [Zone::Command]` on each trigger per CR 114.4
    // (see `oracle_effect/mod.rs`).
    let emblem_spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Daretti's Ultimate", false, DARETTI_EMBLEM_ORACLE)
        .id();

    // Two artifact creatures controlled by P0 that will die simultaneously.
    // Use add_creature (which keeps CoreType::Creature) and then push
    // CoreType::Artifact after building so the golems are creature-artifacts
    // subject to lethal-damage SBA (CR 704.5g) and also trigger the artifact
    // filter on Daretti's emblem.
    let golem_a = scenario.add_creature(P0, "Golem A", 2, 2).id();
    let golem_b = scenario.add_creature(P0, "Golem B", 2, 2).id();

    // An opponent's non-artifact creature that also dies in the same batch.
    // Verifies the filter: the emblem should NOT produce a delayed trigger for
    // non-artifact deaths.
    let opp_bear = scenario.add_vanilla(P1, 2, 2);

    let mut runner = scenario.build();

    // Stamp CoreType::Artifact on both golems now that we have mutable state.
    // Do NOT remove CoreType::Creature — they must remain creature-artifacts
    // so lethal-damage SBA applies (check_lethal_damage requires CoreType::Creature).
    for &id in &[golem_a, golem_b] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        if !obj.card_types.core_types.contains(&CoreType::Artifact) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
        }
    }

    // --- Install the Daretti emblem in the command zone ---
    {
        let state = runner.state_mut();

        // Extract the pre-parsed `Effect::CreateEmblem` from the sorcery object.
        // This avoids re-parsing and ensures `trigger_zones = [Zone::Command]`
        // is set (the parser stamps it on emblem triggers per CR 114.4).
        let emblem_effect = extract_create_emblem_effect(state, emblem_spell_id);

        let dummy_source = ObjectId(9998);
        let emblem_ability = ResolvedAbility::new(emblem_effect, vec![], dummy_source, P0);
        let mut emblem_events = Vec::new();
        engine::game::effects::create_emblem::resolve(state, &emblem_ability, &mut emblem_events)
            .expect("emblem creation must succeed");

        assert_eq!(
            state.command_zone.len(),
            1,
            "emblem must be in the command zone after creation"
        );

        // Verify the trigger has Zone::Command so the command-zone scanner admits it.
        let emblem_id = state.command_zone[0];
        let emblem_obj = &state.objects[&emblem_id];
        assert!(
            emblem_obj.is_emblem,
            "created object must be flagged as emblem"
        );
        let trig = emblem_obj
            .trigger_definitions
            .first()
            .expect("emblem must have at least one trigger definition");
        assert!(
            trig.definition.trigger_zones.contains(&Zone::Command),
            "CR 114.4: emblem trigger must have trigger_zones containing Command, \
             got {:?}",
            trig.definition.trigger_zones
        );
    }

    // --- Simultaneous SBA destruction of both artifacts and the bear ---
    // Mark lethal damage on all three so one SBA pass destroys them in a single
    // simultaneous event batch (CR 704.7). Golems are 2/2, bear is 2/2.
    for &id in &[golem_a, golem_b, opp_bear] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .damage_marked = 2;
    }

    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);

    assert_eq!(
        runner.state().objects.get(&golem_a).unwrap().zone,
        Zone::Graveyard,
        "Golem A must be in graveyard after SBA"
    );
    assert_eq!(
        runner.state().objects.get(&golem_b).unwrap().zone,
        Zone::Graveyard,
        "Golem B must be in graveyard after SBA"
    );
    assert_eq!(
        runner.state().objects.get(&opp_bear).unwrap().zone,
        Zone::Graveyard,
        "opponent bear must be in graveyard after SBA"
    );

    // --- Fire triggers from the SBA events ---
    // CR 603.2c: two distinct ZoneChanged events (one per artifact death) must
    // each independently trigger the emblem. The bear's death does NOT trigger
    // it (artifact + controller filter). So exactly 2 emblem triggers go to the
    // stack.
    engine::game::triggers::process_triggers(runner.state_mut(), &sba_events);

    // CR 603.2c + CR 114.4: two simultaneous P0 artifact deaths must produce
    // two independent trigger instances. The triggers may be on the stack
    // directly (NoChoiceNeeded), or queued in pending_trigger_order awaiting
    // CR 603.3b APNAP ordering choice — count both. The non-artifact bear death
    // must produce no trigger instance.
    let stack_count = runner.state().stack.len();
    let pending_order_count = runner
        .state()
        .pending_trigger_order
        .as_ref()
        .map(|pto| pto.groups.iter().map(|g| g.triggers.len()).sum::<usize>())
        .unwrap_or(0);
    let deferred_count = runner.state().deferred_triggers.len();
    let total_trigger_count = stack_count + pending_order_count + deferred_count;
    assert_eq!(
        total_trigger_count, 2,
        "CR 603.2c + CR 114.4: two simultaneous P0 artifact deaths under the \
         Daretti emblem must produce two independent trigger instances \
         (on stack or in ordering queue); \
         stack={stack_count}, pending_order={pending_order_count}, \
         deferred={deferred_count}"
    );

    // --- Resolve each emblem trigger → creates one delayed trigger per resolution ---
    // Each emblem trigger resolves CreateDelayedTrigger. The resolver uses
    // parent_target_snapshot (reading TriggeringSource from current_trigger_event)
    // to snapshot the specific artifact that died. After resolving both triggers,
    // there must be exactly 2 delayed triggers in state.delayed_triggers.
    drain_stack(&mut runner);

    let delayed_count = runner.state().delayed_triggers.len();
    assert_eq!(
        delayed_count, 2,
        "each Daretti emblem trigger resolution must install one AtNextPhase{{End}} \
         delayed trigger; got {delayed_count} after resolving both emblem triggers"
    );

    // Each delayed trigger must target a distinct artifact (not the bear).
    let delayed_targets: Vec<ObjectId> = runner
        .state()
        .delayed_triggers
        .iter()
        .filter_map(|dt| {
            dt.ability.targets.iter().find_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
        })
        .collect();

    assert!(
        delayed_targets.contains(&golem_a),
        "one delayed trigger must snapshot Golem A; delayed targets: {delayed_targets:?}"
    );
    assert!(
        delayed_targets.contains(&golem_b),
        "one delayed trigger must snapshot Golem B; delayed targets: {delayed_targets:?}"
    );
    assert!(
        !delayed_targets.contains(&opp_bear),
        "the non-artifact bear must not be a delayed trigger target; \
         delayed targets: {delayed_targets:?}"
    );

    // --- Fire delayed triggers at the next end step ---
    // CR 603.7b: each AtNextPhase{End} delayed trigger fires once and is consumed.
    runner.state_mut().phase = Phase::End;
    let end_step_events = vec![GameEvent::PhaseChanged { phase: Phase::End }];
    let stacked =
        engine::game::triggers::check_delayed_triggers(runner.state_mut(), &end_step_events);

    // CR 603.3b (PR-6.75 HIGH-2): two simultaneous same-controller delayed triggered
    // abilities with distinct returns are ordered by their controller before hitting
    // the stack — check_delayed_triggers now routes its firing batch through
    // begin_trigger_ordering (matching the phase-delayed path). The batch pauses on the
    // OrderTriggers choice, so `stacked` is empty until the order is submitted; the
    // drain_stack helper below drains the identity order and resolves both returns.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "CR 603.3b: the two simultaneous AtNextPhase{{End}} Daretti returns must prompt \
         for ordering; waiting_for={:?}",
        runner.state().waiting_for
    );
    let _ = stacked;
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 603.7b: both one-shot delayed triggers must be consumed after firing"
    );

    // --- Resolve the return triggers → both artifacts back on the battlefield ---
    drain_stack(&mut runner);

    assert_eq!(
        runner.state().objects.get(&golem_a).unwrap().zone,
        Zone::Battlefield,
        "CR 603.7 + CR 114.4: Golem A must return to the battlefield at the next \
         end step under Daretti's emblem"
    );
    assert_eq!(
        runner.state().objects.get(&golem_b).unwrap().zone,
        Zone::Battlefield,
        "CR 603.7 + CR 114.4: Golem B must return to the battlefield at the next \
         end step under Daretti's emblem"
    );
    assert_eq!(
        runner.state().objects.get(&opp_bear).unwrap().zone,
        Zone::Graveyard,
        "the non-artifact bear must remain in the graveyard (no delayed trigger \
         was created for it)"
    );
}

/// Sanity guard: a single artifact death under the emblem produces exactly one
/// delayed trigger and that artifact returns at the next end step.
#[test]
fn daretti_emblem_single_artifact_death_returns_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emblem_spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Daretti's Ultimate", false, DARETTI_EMBLEM_ORACLE)
        .id();

    let lone_golem = scenario.add_creature(P0, "Lone Golem", 2, 2).id();

    let mut runner = scenario.build();

    // Stamp CoreType::Artifact without removing Creature — the golem must be a
    // creature-artifact for both lethal-damage SBA (CR 704.5g) and Daretti's
    // emblem artifact filter.
    {
        let obj = runner.state_mut().objects.get_mut(&lone_golem).unwrap();
        if !obj.card_types.core_types.contains(&CoreType::Artifact) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
        }
    }

    // Install Daretti's emblem.
    {
        let state = runner.state_mut();
        let emblem_effect = extract_create_emblem_effect(state, emblem_spell_id);
        let dummy_source = ObjectId(9998);
        let emblem_ability = ResolvedAbility::new(emblem_effect, vec![], dummy_source, P0);
        let mut emblem_events = Vec::new();
        engine::game::effects::create_emblem::resolve(state, &emblem_ability, &mut emblem_events)
            .expect("emblem creation must succeed");
    }

    runner
        .state_mut()
        .objects
        .get_mut(&lone_golem)
        .unwrap()
        .damage_marked = 2;

    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);
    engine::game::triggers::process_triggers(runner.state_mut(), &sba_events);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "single artifact death must put exactly one emblem trigger on the stack"
    );

    drain_stack(&mut runner);

    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "single artifact death must install exactly one delayed trigger"
    );
    assert_eq!(
        runner.state().delayed_triggers[0]
            .ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            }),
        Some(lone_golem),
        "the delayed trigger must snapshot the lone golem"
    );

    runner.state_mut().phase = Phase::End;
    let stacked = engine::game::triggers::check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged { phase: Phase::End }],
    );
    assert_eq!(
        stacked.len(),
        1,
        "exactly one delayed trigger fires at end step"
    );
    drain_stack(&mut runner);

    assert_eq!(
        runner.state().objects.get(&lone_golem).unwrap().zone,
        Zone::Battlefield,
        "the golem must return to the battlefield at the next end step"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot delayed trigger must be consumed"
    );
}
