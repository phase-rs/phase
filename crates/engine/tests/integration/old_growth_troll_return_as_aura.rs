//! Integration test for the Old-Growth Troll return-as-Aura class
//! (GitHub issue #950).
//!
//! Drives the full pipeline end-to-end through the public `apply()` engine
//! surface to verify that no part of the implementation can be silently
//! bypassed by a future refactor:
//!
//!   1. A creature with a "When ~ dies, return it to the battlefield. It's an
//!      Aura enchantment with enchant Forest you control and \"...\"" trigger
//!      is on the battlefield with a Forest available to enchant.
//!   2. The creature is moved to the graveyard via the real zone-move
//!      pipeline so the dies trigger (CR 603.6c) fires.
//!   3. The engine resolves through the `ChangeZone` (return-to-battlefield)
//!      sub-effect and then the `ReturnAsAura` resolver.
//!   4. With exactly one legal target (the Forest), the resolver attaches
//!      the returned object directly without prompting (single-candidate
//!      auto-attach path).
//!   5. We assert that the returned permanent is now an Aura attached to the
//!      Forest, with the `Enchant(Forest you control)` keyword installed
//!      via the transient continuous effect (CR 614.1 + CR 303.4 +
//!      CR 613.1d + CR 613.1f).
//!
//! This complements the unit tests in
//! `crates/engine/src/game/effects/return_as_aura.rs::tests` which exercise
//! `resolve()` directly with a hand-built `ResolvedAbility`. The risk those
//! tests don't catch: if a future refactor of
//! `extract_target_filter_from_effect` accidentally re-enables target-slot
//! creation for `ReturnAsAura`, or if the chunk-splitter / IR fold drifts,
//! the unit tests will still pass while the real card breaks. This test
//! drives the trigger pipeline so any pipeline-level regression surfaces.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::ContinuousModification;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::zones::Zone;

/// Old-Growth Troll's full Oracle text. The dies trigger is the load-bearing
/// piece; the leading "Trample" keyword is included so the parser sees the
/// canonical card shape (keyword line + trigger line).
const OLD_GROWTH_TROLL_ORACLE: &str = "Trample\n\
When ~ dies, if it was a creature, return it to the battlefield. It's an Aura \
enchantment with enchant Forest you control and \"Enchanted Forest has '{T}: \
Add {G}{G}' and '{1}, {T}, Sacrifice this land: Create a tapped 4/4 green \
Troll Warrior creature token with trample.'\"";

/// Place a Forest (Land + Basic + Forest subtype) on the battlefield under
/// the given player. Returns the object id. Operates directly on the runner's
/// `GameState` (the public `GameScenario::add_basic_land` helper installs a
/// mana ability and the supertype but does not push the "Forest" subtype, which
/// the enchant filter relies on).
fn add_forest(
    state: &mut engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> engine::types::identifiers::ObjectId {
    use engine::game::zones::create_object;
    use engine::types::card_type::Supertype;
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        player,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.supertypes.push(Supertype::Basic);
    obj.card_types.subtypes.push("Forest".to_string());
    obj.base_card_types = obj.card_types.clone();
    obj.entered_battlefield_turn = obj.entered_battlefield_turn.or(Some(0));
    obj.summoning_sick = false;
    id
}

/// Drive the engine until either the waiting_for is a stack-empty Priority
/// state or a guard fires. Auto-passes priority on all priority-style waiting
/// states so triggers can resolve.
fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain_to_priority exceeded its safety bound; last waiting_for = {:?}, stack = {}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    // Cannot pass priority — either the engine is awaiting
                    // a non-priority action we don't know how to feed, or
                    // the game has ended. Either way, stop draining and let
                    // the caller assert on whatever state is reachable.
                    break;
                }
            }
        }
    }
}

#[test]
fn old_growth_troll_dies_to_graveyard_returns_as_aura_attached_to_forest() {
    // P0 controls the about-to-die creature AND the Forest that will be the
    // sole legal enchant candidate (filter is "enchant Forest you control").
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // The creature: a 4/4 with Trample (placeholder body) carrying the dies
    // trigger parsed from the Old-Growth Troll Oracle text. We don't need
    // mana costs; we only need the trigger and the creature type. The card
    // name is the source for `~` normalization.
    let troll_id = scenario
        .add_creature_from_oracle(P0, "Old-Growth Troll", 4, 4, OLD_GROWTH_TROLL_ORACLE)
        .id();

    let mut runner = scenario.build();
    let forest_id = add_forest(runner.state_mut(), P0);

    // Precondition sanity: the troll has at least one trigger installed
    // (the dies trigger from the Oracle text). We can't enumerate via
    // `iter_all()` from outside the crate, but a non-empty
    // `trigger_definitions` confirms the parser produced something.
    {
        let troll = &runner.state().objects[&troll_id];
        assert!(
            !troll.trigger_definitions.is_empty(),
            "Old-Growth Troll should have at least one trigger installed by \
             the Oracle text parser"
        );
    }

    // Move the troll to the graveyard via the real zone-move pipeline. This
    // emits a `ZoneChanged { from: Battlefield, to: Graveyard }` event; the
    // dies trigger (CR 603.6c) is collected by `process_triggers` from that
    // event and pushed onto the stack. We then resolve the stack via
    // `apply` (PassPriority) which drives the ChangeZone + ReturnAsAura
    // sub-effects through the real resolver pipeline.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), troll_id, Zone::Graveyard, &mut events);

    // Precondition: troll moved to graveyard.
    assert_eq!(
        runner.state().objects[&troll_id].zone,
        Zone::Graveyard,
        "precondition: the troll must have moved to the graveyard"
    );

    // Collect the dies trigger from the ZoneChanged event and place it on
    // the stack (the equivalent of what `run_post_action_pipeline` does
    // during a normal `apply` cycle).
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the dies trigger should be on the stack after process_triggers; \
         stack.len() = {}, waiting_for = {:?}",
        runner.state().stack.len(),
        runner.state().waiting_for,
    );

    // Drain priority/trigger resolution until the dies trigger has fully
    // resolved. With exactly one legal target (the single Forest), the
    // ReturnAsAura resolver takes the auto-attach path and never installs
    // `WaitingFor::ReturnAsAuraTarget`.
    drain_to_priority(&mut runner);

    // The "returned" object is the SAME ObjectId as the original troll on
    // re-entry. `last_zone_changed_ids` placed it back in battlefield, and
    // ChangeZone preserves the id. (If a future refactor changes id
    // identity on re-entry, this assertion will catch it.)
    let returned = runner
        .state()
        .objects
        .get(&troll_id)
        .expect("the troll's object record must still exist");

    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "after the ReturnAsAura sub-effect, the returned object must be on \
         the battlefield"
    );

    // Layer 4 (CR 613.1d): the returned object should be an Enchantment with
    // the Aura subtype, not a Creature. The transient continuous effect must
    // have been installed AND evaluated by the layer system.
    assert!(
        returned
            .card_types
            .core_types
            .contains(&CoreType::Enchantment),
        "returned object should be an Enchantment after the layer system \
         applies SetCardTypes; got core_types = {:?}",
        returned.card_types.core_types
    );
    assert!(
        returned.card_types.subtypes.iter().any(|s| s == "Aura"),
        "returned object should have the Aura subtype; got subtypes = {:?}",
        returned.card_types.subtypes
    );
    assert!(
        !returned.card_types.core_types.contains(&CoreType::Creature),
        "returned object should no longer be a Creature; got core_types = {:?}",
        returned.card_types.core_types
    );

    // Layer 6 (CR 613.1f + CR 702.5a): the Enchant keyword must be installed
    // and visible on the returned object.
    let has_enchant_kw = returned
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Enchant(_)));
    assert!(
        has_enchant_kw,
        "returned object should have the Enchant(...) keyword; got keywords = {:?}",
        returned.keywords
    );

    // CR 701.3 + CR 303.4: the returned object must be attached to the Forest.
    assert_eq!(
        returned.attached_to,
        Some(forest_id.into()),
        "returned object should be attached to the Forest"
    );

    // The Forest's reciprocal `attachments` list must include the returned
    // object — silent no-op in `attach_to` would leave this empty.
    let forest = &runner.state().objects[&forest_id];
    assert!(
        forest.attachments.contains(&troll_id),
        "Forest.attachments should contain the returned object (id={troll_id:?}); \
         got {:?}",
        forest.attachments
    );

    // The TransientContinuousEffect carrying the Aura's continuous effect
    // must still be installed (Duration::UntilHostLeavesPlay).
    assert!(
        !runner.state().transient_continuous_effects.is_empty(),
        "Aura continuous effect should still be installed; \
         transient_continuous_effects is empty"
    );
    let installed = runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|tce| {
            tce.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Enchant(_)
                    }
                )
            })
        });
    assert!(
        installed,
        "no TransientContinuousEffect with an Enchant(...) AddKeyword \
         modification found; effects = {:?}",
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .map(|tce| tce
                .modifications
                .iter()
                .map(|m| format!("{m:?}"))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );

    // Stack should be empty — the dies trigger fully resolved through the
    // ChangeZone + ReturnAsAura chain. (If `ReturnAsAura` accidentally
    // installed `WaitingFor::ReturnAsAuraTarget` despite having only one
    // candidate, that bug would surface as a non-empty stack or a
    // ReturnAsAuraTarget waiting state at this point.)
    assert!(
        runner.state().stack.is_empty(),
        "stack should be empty after the dies trigger resolves; \
         stack.len() = {}",
        runner.state().stack.len()
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "with exactly one legal target, the resolver MUST auto-attach and \
         not install WaitingFor::ReturnAsAuraTarget; current waiting_for = {:?}",
        runner.state().waiting_for
    );
}
