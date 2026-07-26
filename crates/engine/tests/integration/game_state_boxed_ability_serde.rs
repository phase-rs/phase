//! Serde transparency for the boxed `ResolvedAbility` storage sites.
//!
//! `StackEntryKind::Spell.ability`, `StackEntryKind::ActivatedAbility.ability`
//! and `GameState::pending_trigger` were retyped from inline values to
//! `Box<_>` to cut `GameState`'s inline stack footprint. Persisted sessions and
//! host checkpoints cross the wire through exactly these fields, so the wire
//! shape must not have moved.
//!
//! The existing `game_state_serializes_and_roundtrips` unit test cannot see
//! this: it round-trips `GameState::default()`, whose stack is empty and whose
//! `pending_trigger` is `None`, so every retyped field is degenerate there and
//! the test would pass for any layout. These fixtures are populated instead.
//!
//! The discriminator is `StackEntryKind::TriggeredAbility.ability`, which was
//! **already** `Box<ResolvedAbility>` before this change. Asserting that the
//! two newly-boxed spellings serialize to the *same JSON shape* as the
//! long-boxed one is what makes these assertions non-vacuous: if `Box<T>` were
//! not serde-transparent, the already-boxed control would move in lockstep with
//! the newly-boxed fields and the shape comparison would still hold — so the
//! test additionally pins the concrete key path, which only holds if `Box`
//! introduces no wrapper level at all.

use engine::game::scenario::{P0, P1};
use engine::game::triggers::PendingTrigger;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::game_state::{GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};

const SOURCE: ObjectId = ObjectId(700);

fn damage_ability() -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P1)],
        SOURCE,
        P0,
    )
}

/// A state whose every retyped field is populated — the opposite of the
/// `GameState::default()` fixture the pre-existing round-trip test uses.
fn populated_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.stack.push_back(StackEntry {
        id: ObjectId(701),
        source_id: SOURCE,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(Box::new(damage_ability())),
            casting_variant: Default::default(),
            actual_mana_spent: 2,
        },
    });
    state.stack.push_back(StackEntry {
        id: ObjectId(702),
        source_id: SOURCE,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: SOURCE,
            ability: Box::new(damage_ability()),
        },
    });
    state.stack.push_back(StackEntry {
        id: ObjectId(703),
        source_id: SOURCE,
        controller: P0,
        kind: StackEntryKind::TriggeredAbility {
            source_id: SOURCE,
            ability: Box::new(damage_ability()),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    });
    state.pending_trigger = Some(Box::new(PendingTrigger {
        source_id: SOURCE,
        controller: P0,
        condition: None,
        ability: Box::new(damage_ability()),
        timestamp: 9,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: Vec::new(),
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    }));
    state
}

#[test]
fn boxed_abilities_round_trip_through_serde() {
    let state = populated_state();

    // Reach-guard: the fixture really does populate every retyped field, so a
    // later refactor cannot quietly degenerate this back into the default-state
    // test it exists to replace.
    assert_eq!(state.stack.len(), 3, "reach-guard: three stack entries");
    assert!(
        state.stack.iter().all(|entry| entry.ability().is_some()),
        "reach-guard: every stack entry carries a populated ability"
    );
    assert!(
        state.pending_trigger.is_some(),
        "reach-guard: pending_trigger is populated"
    );

    let json = serde_json::to_string(&state).expect("populated state serializes");
    let mut restored: GameState = serde_json::from_str(&json).expect("and deserializes");
    restored.rng = state.rng.clone(); // skipped by serde; not under test here

    assert_eq!(
        state, restored,
        "a state with populated boxed abilities must survive a serde round trip"
    );
}

#[test]
fn boxing_introduces_no_wrapper_level_in_the_wire_shape() {
    let value = serde_json::to_value(populated_state()).expect("state serializes");
    let stack = value["stack"].as_array().expect("stack is an array");

    // The already-boxed `TriggeredAbility` is the control: its wire shape did
    // not change in this commit, so it defines what "unchanged" looks like.
    let control = &stack[2]["kind"]["data"]["ability"];
    assert!(
        control.get("effect").is_some(),
        "control: the long-boxed TriggeredAbility.ability serializes as a bare \
         ResolvedAbility object, got {control}"
    );

    // The two newly-boxed sites must match that shape exactly — no `Box`
    // wrapper, no extra nesting level, same key path.
    for (index, label) in [(0usize, "Spell"), (1, "ActivatedAbility")] {
        let ability = &stack[index]["kind"]["data"]["ability"];
        assert!(
            ability.get("effect").is_some(),
            "{label}.ability must serialize as a bare ResolvedAbility object \
             (no Box wrapper level), got {ability}"
        );
        assert_eq!(
            ability, control,
            "{label}.ability must serialize identically to the already-boxed \
             TriggeredAbility.ability"
        );
    }

    // `pending_trigger` is `#[serde(default)]` and crosses the wire; its inner
    // ability is boxed twice over (field and struct member) and must still be
    // flat.
    assert!(
        value["pending_trigger"]["ability"].get("effect").is_some(),
        "pending_trigger.ability must serialize as a bare ResolvedAbility object, got {}",
        value["pending_trigger"]
    );

    // `pending_discard_for_cost` is `#[serde(skip)]`; boxing must not have
    // promoted it onto the wire.
    assert!(
        value.get("pending_discard_for_cost").is_none(),
        "pending_discard_for_cost is #[serde(skip)] and must stay off the wire"
    );
}
