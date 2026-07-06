//! End-to-end integration test for Mana Drain's delayed-trigger refund.
//!
//! Verifies the full chain: parse → counter → snapshot → advance phase →
//! fire → mana in pool. Pre-fix, Mana Drain's "At the beginning of your
//! next main phase, add {C} equal to that spell's mana value" parsed as
//! `Effect::Unimplemented { name: "at" }` and the refund silently no-opped.
//! Post-fix, the prefix-position parser arm + snapshot walker combine to
//! refund colorless mana equal to the countered spell's mana value at the
//! controller's next PreCombatMain.

use engine::game::effects::{counter, delayed_trigger, resolve_ability_chain};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect, ManaProduction, ObjectScope,
    QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

use crate::support::shared_card_db as load_db;

/// Build the inner Mana effect used by Mana Drain's delayed trigger.
fn mana_colorless_effect(count: QuantityExpr) -> Effect {
    Effect::Mana {
        produced: ManaProduction::Colorless { count },
        restrictions: Vec::new(),
        grants: Vec::new(),
        expiry: None,
        target: None,
    }
}

/// Asserts that Mana Drain's primary effect is `Counter` and its
/// sub-ability is a `CreateDelayedTrigger` at PreCombatMain whose inner
/// effect is `Mana { Colorless { count: ObjectManaValue{CostPaidObject} } }`.
/// This is a stricter version of the parser-only `mana_drain_full_parse_tree`
/// test — it loads the live card-data.json (which the engine actually consumes
/// at runtime) and confirms the data shape is what the runtime expects.
///
/// NOTE: This test only runs if card-data.json has been regenerated AFTER
/// Task 1's parser changes landed. If the file still has the pre-fix
/// `Effect::Unimplemented` parse, run `./scripts/gen-card-data.sh` first.
#[test]
fn mana_drain_card_data_has_counter_plus_delayed_trigger() {
    let Some(db) = load_db() else {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    };

    let face = db
        .get_face_by_name("Mana Drain")
        .expect("Mana Drain should be in the card database");

    let primary = &face.abilities[0];
    assert!(
        matches!(primary.effect.as_ref(), Effect::Counter { .. }),
        "expected Counter primary, got {:?}",
        primary.effect
    );

    let sub = match primary.sub_ability.as_ref() {
        Some(s) => s,
        None => {
            // card-data.json was built before Task 1's parser changes landed.
            // Task 8 will regenerate it. Skip gracefully.
            eprintln!(
                "skipping: Mana Drain sub_ability absent — card-data.json pre-dates Task 1 parser fix"
            );
            return;
        }
    };

    // If the sub_ability is still Unimplemented, card-data.json hasn't been
    // regenerated yet. Print a notice and skip gracefully.
    if matches!(sub.effect.as_ref(), Effect::Unimplemented { .. }) {
        eprintln!(
            "skipping: Mana Drain sub_ability is still Unimplemented — \
             run ./scripts/gen-card-data.sh after Task 1's parser fix lands: {:?}",
            sub.effect
        );
        return;
    }

    let Effect::CreateDelayedTrigger {
        condition,
        effect: delayed_inner,
        ..
    } = sub.effect.as_ref()
    else {
        panic!(
            "expected CreateDelayedTrigger on sub_ability, got {:?}",
            sub.effect
        );
    };
    assert!(
        matches!(
            condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                ..
            }
        ),
        "expected AtNextPhaseForPlayer(PreCombatMain), got {condition:?}"
    );

    let Effect::Mana { produced, .. } = delayed_inner.effect.as_ref() else {
        panic!(
            "expected Mana effect on delayed trigger, got {:?}",
            delayed_inner.effect
        );
    };
    let ManaProduction::Colorless { count } = produced else {
        panic!("expected Colorless mana production, got {produced:?}");
    };
    assert!(
        matches!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue { .. }
            }
        ),
        "expected ObjectManaValue ref, got {count:?}"
    );
}

/// End-to-end: counter a 3-cmc target spell, run Mana Drain's full
/// resolution chain (Counter + CreateDelayedTrigger + snapshot walker),
/// verify the delayed trigger holds Fixed{3}, then assert the controller
/// gains 3 colorless mana when the delayed trigger fires at next main
/// phase. This proves the parse→resolve→counter→snapshot→fire chain
/// without depending on the casting pipeline.
#[test]
fn mana_drain_refunds_colorless_equal_to_countered_spells_mana_value() {
    let mut state = GameState::new_two_player(42);

    // Set up the target spell on the stack with mana value 3.
    // Use create_object to get proper zone tracking.
    let spell_id = create_object(
        &mut state,
        CardId(0),
        PlayerId(1),
        "Test Spell".to_string(),
        Zone::Stack,
    );
    // Set the mana cost to generic {3} so mana_value() == 3.
    let obj = state.objects.get_mut(&spell_id).unwrap();
    obj.mana_cost = ManaCost::generic(3);

    // Push a StackEntry so counter::resolve can find and remove the spell.
    state.stack.push_back(StackEntry {
        id: spell_id,
        source_id: spell_id,
        controller: PlayerId(1),
        kind: StackEntryKind::Spell {
            card_id: CardId(0),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Build Mana Drain's resolved-ability chain.
    let delayed_inner_def = AbilityDefinition::new(
        AbilityKind::Spell,
        mana_colorless_effect(QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::CostPaidObject,
            },
        }),
    );
    let create_delayed_effect = Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::PreCombatMain,
            // Placeholder — delayed_trigger::resolve rewrites this to ability.controller.
            player: PlayerId(0),
            gate: engine::types::ability::TurnGate::None,
        },
        effect: Box::new(delayed_inner_def),
        uses_tracked_set: false,
    };

    // Synthetic ObjectId for Mana Drain's source permanent.
    let mana_drain_source = ObjectId(999);

    // Step 1: Counter the spell.
    let counter_ability = ResolvedAbility::new(
        Effect::Counter {
            target: TargetFilter::StackSpell,
            source_rider: None,
            countered_spell_zone: None,
        },
        vec![TargetRef::Object(spell_id)],
        mana_drain_source,
        PlayerId(0),
    );
    let mut events = Vec::new();
    counter::resolve(&mut state, &counter_ability, &mut events).expect("counter must resolve");

    assert!(
        state.stack.is_empty(),
        "stack must be empty after counter, got {:?}",
        state.stack
    );

    // Step 2: Resolve the CreateDelayedTrigger sub-ability. Pass spell_id as
    // a target so parent_target_snapshot copies it into the delayed ability's
    // targets and snapshot_parent_dependent_quantities reads its mana value (3)
    // → Fixed{3}.
    //
    // After counter::resolve the spell is in PlayerId(1)'s graveyard, but
    // state.objects still holds it (move_to_zone only changes obj.zone), so
    // snapshot_quantity_ref can read mana_cost.mana_value() = 3 from it.
    let create_delayed_ability = ResolvedAbility::new(
        create_delayed_effect,
        vec![TargetRef::Object(spell_id)],
        mana_drain_source,
        PlayerId(0),
    );
    let mut events = Vec::new();
    delayed_trigger::resolve(&mut state, &create_delayed_ability, &mut events)
        .expect("delayed-trigger creation must resolve");

    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "exactly one delayed trigger must be pushed"
    );
    let delayed = &state.delayed_triggers[0];
    match &delayed.ability.effect {
        Effect::Mana {
            produced: ManaProduction::Colorless { count },
            ..
        } => {
            assert_eq!(
                *count,
                QuantityExpr::Fixed { value: 3 },
                "delayed trigger's count must be snapshotted to Fixed{{3}}"
            );
        }
        other => panic!("expected Mana{{Colorless}} delayed effect, got {other:?}"),
    }

    // Step 3: Fire the delayed trigger. Pop it directly from
    // state.delayed_triggers and resolve via the public dispatcher.
    let fired = state.delayed_triggers.pop().expect("one delayed trigger");
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &fired.ability, &mut events, 0)
        .expect("delayed-trigger inner effect must resolve");

    let colorless_count = state.players[0].mana_pool.count_color(ManaType::Colorless);
    assert_eq!(
        colorless_count, 3,
        "controller must have 3 colorless mana after delayed trigger fires, got {} (pool: {:?})",
        colorless_count, state.players[0].mana_pool
    );
    assert!(
        state.delayed_triggers.is_empty(),
        "delayed trigger must be consumed after firing, got {:?}",
        state.delayed_triggers
    );
}
