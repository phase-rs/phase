//! CR 514.2 + CR 514.3a + CR 613.1b runtime coverage for the "when you lose
//! control of that <permanent> this turn" delayed-trigger seam (the B1 runtime
//! half of Stolen Uniform. As of block-D + fix#2 the card's parser is fully
//! supported (front half B3 + the last-sentence lose-control delayed trigger);
//! this file's hand-installed trigger remains a valid focused runtime unit).
//!
//! These drive the REAL cleanup / priority / delayed-trigger-dispatch pipeline
//! (`GameRunner::advance_to_phase` → `auto_advance` → `execute_cleanup`, and the
//! shared `check_delayed_triggers`). The delayed trigger is hand-installed
//! because no supported card currently *creates* one via a cast (that is the
//! deferred parser work); every ASSERTION below flips if a B1 change is reverted:
//!
//!   * emit-on-expiry (`turns::execute_cleanup`) + CR 514.3a re-entry
//!     (`priority.rs`): without the emitted `ControllerChanged` the loss is
//!     silent and the trigger never fires — `fires_at_cleanup` fails.
//!   * `valid_card` scoping (`match_changes_controller`): without it the matcher
//!     fires on ANY object's control change — `unrelated_loss_does_not_fire`
//!     (the Portent trap) fails.
//!   * loss-direction gate (`match_changes_controller`): without it a *gain* to
//!     you fires too — `gain_to_you_does_not_fire` fails.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::{check_delayed_triggers, trigger_source_context_for_latch};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, DelayedTriggerCondition,
    DelayedTriggerLifetime, Duration, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
};
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::DelayedTrigger;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Make `id` a noncreature artifact so it neither dies to the 0-toughness SBA
/// nor triggers a combat declare-attackers prompt (isolates the control seam).
fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types = CardType::default();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
}

/// Move `id` into `owner`'s graveyard (mimics a resolved spell source).
fn move_to_graveyard(runner: &mut GameRunner, id: ObjectId, owner: PlayerId) {
    let st = runner.state_mut();
    st.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
    st.battlefield.retain(|&x| x != id);
    st.players
        .iter_mut()
        .find(|p| p.id == owner)
        .unwrap()
        .graveyard
        .push_back(id);
}

fn gain_until_eot(runner: &mut GameRunner, source: ObjectId, gainer: PlayerId, obj: ObjectId) {
    runner.state_mut().add_transient_continuous_effect(
        source,
        gainer,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: obj },
        vec![ContinuousModification::ChangeController],
        None,
    );
    engine::game::layers::flush_layers(runner.state_mut());
}

/// A one-shot ThisTurn "when you lose control of `scoped_obj` this turn, draw a
/// card" delayed trigger controlled by `controller`, sourced from `source`.
fn install_lose_control_draw(
    runner: &mut GameRunner,
    source: ObjectId,
    controller: PlayerId,
    scoped_obj: ObjectId,
) {
    let inner = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let mut ability =
        engine::game::ability_utils::build_resolved_from_def(&inner, source, controller);
    ability.set_trigger_source_recursive(trigger_source_context_for_latch(
        runner.state(),
        runner
            .state()
            .objects
            .get(&source)
            .expect("delayed trigger source"),
    ));
    let mut trig = TriggerDefinition::new(TriggerMode::ChangesController);
    trig.valid_card = Some(TargetFilter::SpecificObject { id: scoped_obj });
    trig.execute = None;
    runner.state_mut().delayed_triggers.push(DelayedTrigger {
        condition: DelayedTriggerCondition::WhenNextEvent {
            trigger: Box::new(trig),
            or_trigger: None,
            lifetime: DelayedTriggerLifetime::ThisTurn,
        },
        ability,
        controller,
        source_id: source,
        one_shot: true,
    });
}

/// Install a SELF-REFERENTIAL "when you lose control of ~, draw a card"
/// ChangesController trigger directly on battlefield permanent `obj` (mirrors the
/// Khârn / Duplicity / Gustha's Scepter shape: `valid_card = SelfRef`, so the
/// trigger source IS the changing object). Unlike the delayed-trigger helper
/// above (`SpecificObject`, `source_id != object_id`), this exercises the
/// `source_id == object_id` branch of `match_changes_controller`.
fn install_selfref_lose_control_draw(runner: &mut GameRunner, obj: ObjectId) {
    let mut trig = TriggerDefinition::new(TriggerMode::ChangesController);
    trig.valid_card = Some(TargetFilter::SelfRef);
    trig.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )));
    let o = runner.state_mut().objects.get_mut(&obj).unwrap();
    o.base_trigger_definitions = Arc::new(vec![trig.clone()]);
    o.trigger_definitions = vec![trig].into();
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .hand
        .len()
}

/// CR 514.2 + CR 514.3a + CR 613.1b: when the until-EOT control effect ends at
/// cleanup, control reverts to the owner, the loss emits a `ControllerChanged`,
/// and the scoped "lose control this turn" delayed trigger fires during a new
/// cleanup step (the draw resolves). REVERT-PROBE: remove the emit-on-expiry OR
/// the CR 514.3a re-entry and the trigger never resolves — both asserts flip.
#[test]
fn lose_control_this_turn_delayed_fires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    let source = scenario.add_creature(P0, "Stolen Uniform", 0, 0).id();
    let sword = scenario.add_creature(P1, "Sword", 2, 2).id();
    let mut runner = scenario.build();

    make_artifact(&mut runner, sword);
    move_to_graveyard(&mut runner, source, P0);
    gain_until_eot(&mut runner, source, P0, sword);
    assert_eq!(
        runner.state().objects[&sword].controller,
        P0,
        "P0 temporarily controls the Sword"
    );
    install_lose_control_draw(&mut runner, source, P0, sword);

    let before = hand_size(&runner, P0);
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&sword].controller,
        P1,
        "control reverts to owner P1 at cleanup"
    );
    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "the lose-control delayed trigger must fire at cleanup and draw"
    );
}

/// CR 603.10d + CR 613.1b: a SELF-REFERENTIAL "when you lose control of ~"
/// trigger (the source IS the changing object, `valid_card = SelfRef`) must fire
/// when its controller loses control. Drives the real until-EOT-expiry →
/// `ControllerChanged` → `process_triggers` pipeline. `collect_pending_triggers`
/// flushes layers BEFORE scanning, so the permanent's live controller is already
/// the NEW controller at match time — the old single-line
/// `source.controller == old_controller` gate therefore wrongly rejected EVERY
/// self-ref loss (100% of currently-supported "when you lose control of ~"
/// cards). The fix returns TRUE on the `source_id == object_id` branch (CR 603.10d
/// look-back: the ability is intrinsically the pre-change controller's).
/// REVERT-PROBE (measured): restore the single-line gate and the draw never
/// happens — the final assert flips from `before + 1` to `before`.
#[test]
fn selfref_lose_control_this_turn_fires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B"]);
    // Owned by P1; the self-ref lose-control trigger lives on this permanent.
    let permanent = scenario.add_creature(P1, "Self Ref Artifact", 0, 0).id();
    let mut runner = scenario.build();

    make_artifact(&mut runner, permanent);
    install_selfref_lose_control_draw(&mut runner, permanent);
    // P0 steals it until EOT: P0 controls it now; control reverts to owner P1 at
    // cleanup, at which point P0 loses control (the self-ref loss event).
    gain_until_eot(&mut runner, permanent, P0, permanent);
    assert_eq!(
        runner.state().objects[&permanent].controller,
        P0,
        "P0 temporarily controls the permanent"
    );

    // Attribution-independent signal: the fired trigger draws exactly one card
    // (control-change triggers attribute to the post-flush controller; who draws
    // is out of scope — that the trigger FIRES at all is the regression here).
    let before = hand_size(&runner, P0) + hand_size(&runner, P1);
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&permanent].controller,
        P1,
        "control reverts to owner P1 at cleanup (P0 loses control)"
    );
    assert_eq!(
        hand_size(&runner, P0) + hand_size(&runner, P1),
        before + 1,
        "the self-ref lose-control trigger must fire on the loss and draw a card"
    );
}

/// CR 514.2 + CR 613.1b: multiple expiring control effects on the same
/// permanent create one controller reversion, so cleanup must emit one
/// `ControllerChanged` event for that object. REVERT-PROBE: remove the cleanup
/// dedupe and the self-ref trigger fires twice, drawing two cards.
#[test]
fn duplicate_control_reversions_emit_one_loss_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B"]);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B"]);
    let permanent = scenario.add_creature(P1, "Self Ref Artifact", 0, 0).id();
    let extra_source = scenario
        .add_creature(P0, "Second Control Effect", 0, 0)
        .id();
    let mut runner = scenario.build();

    make_artifact(&mut runner, permanent);
    make_artifact(&mut runner, extra_source);
    install_selfref_lose_control_draw(&mut runner, permanent);

    gain_until_eot(&mut runner, permanent, P0, permanent);
    gain_until_eot(&mut runner, extra_source, P0, permanent);
    assert_eq!(
        runner.state().objects[&permanent].controller,
        P0,
        "P0 temporarily controls the permanent"
    );

    let before = hand_size(&runner, P0) + hand_size(&runner, P1);
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&permanent].controller,
        P1,
        "control reverts to owner P1 at cleanup"
    );
    assert_eq!(
        hand_size(&runner, P0) + hand_size(&runner, P1),
        before + 1,
        "duplicate expiring control effects must publish exactly one loss event"
    );
}

/// Portent trap: a scoped trigger must NOT fire on an UNRELATED object's control
/// change. The trigger is bound to `decoy` (never changes control); only the
/// Sword's control reverts at cleanup. REVERT-PROBE: drop `valid_card` scoping in
/// `match_changes_controller` and the matcher fires on any `ControllerChanged`,
/// drawing a card — this `+ 0` assert flips.
#[test]
fn unrelated_control_loss_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    let source = scenario.add_creature(P0, "Stolen Uniform", 0, 0).id();
    let sword = scenario.add_creature(P1, "Sword", 2, 2).id();
    let decoy = scenario.add_creature(P0, "Decoy", 2, 2).id();
    let mut runner = scenario.build();

    make_artifact(&mut runner, sword);
    make_artifact(&mut runner, decoy);
    move_to_graveyard(&mut runner, source, P0);
    // P0 controls the Sword until EOT; the trigger is scoped to the unrelated decoy.
    gain_until_eot(&mut runner, source, P0, sword);
    install_lose_control_draw(&mut runner, source, P0, decoy);

    let before = hand_size(&runner, P0);
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&sword].controller,
        P1,
        "the Sword still reverts to owner P1"
    );
    assert_eq!(
        hand_size(&runner, P0),
        before,
        "a control change on an UNRELATED object must not fire the decoy-scoped trigger"
    );
}

/// Loss-direction gate: "when you lose control" fires on the loss (old == you),
/// never on the gain (new == you). Drives the real `check_delayed_triggers`
/// dispatcher with each direction. REVERT-PROBE: drop the direction check in
/// `match_changes_controller` and the gain event fires the trigger too — the
/// first `delayed_triggers.len() == 1` assert flips.
#[test]
fn gain_to_you_does_not_fire_only_loss() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A"]);
    let source = scenario.add_creature(P0, "Stolen Uniform", 0, 0).id();
    let sword = scenario.add_creature(P1, "Sword", 2, 2).id();
    let mut runner = scenario.build();
    make_artifact(&mut runner, sword);
    move_to_graveyard(&mut runner, source, P0);
    install_lose_control_draw(&mut runner, source, P0, sword);

    // GAIN to P0 (old = owner P1, new = P0): P0 is not losing control — no fire.
    let gain = GameEvent::ControllerChanged {
        object_id: sword,
        old_controller: P1,
        new_controller: P0,
    };
    let mut events = vec![gain];
    let out = check_delayed_triggers(runner.state_mut(), &events);
    events.extend(out);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "a gain TO the trigger's controller must not fire a lose-control trigger"
    );
    assert!(
        runner.state().stack.is_empty(),
        "no trigger on the stack after a gain"
    );

    // LOSS from P0 (old = P0, new = owner P1): P0 loses control — fires.
    let loss = GameEvent::ControllerChanged {
        object_id: sword,
        old_controller: P0,
        new_controller: P1,
    };
    let _ = check_delayed_triggers(runner.state_mut(), &[loss]);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the loss event fires (and consumes) the one-shot lose-control trigger"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the fired trigger is on the stack"
    );
}
