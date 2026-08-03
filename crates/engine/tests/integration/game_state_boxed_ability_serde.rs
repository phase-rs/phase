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
//! **What actually discriminates here, and what does not.**
//!
//! `boxing_introduces_no_wrapper_level_in_the_wire_shape` makes two different
//! kinds of assertion, and only one of them can fail on its own:
//!
//! * **The discriminator: the key-path assertions.**
//!   `stack[i]["kind"]["data"]["ability"].get("effect").is_some()` — and the
//!   same for `pending_trigger.ability` — is what proves `Box` adds no wrapper
//!   level. If `Box<T>` serialized as anything other than `T` (a `{"Box": …}`
//!   tag, a one-element sequence, an extra nesting level), `"effect"` would not
//!   be a direct child at that path and these assertions go red. They are
//!   absolute, not relative: nothing in the fixture can make them pass
//!   vacuously.
//!
//! * **Necessary but insufficient: the control comparison.**
//!   `StackEntryKind::TriggeredAbility.ability` was **already**
//!   `Box<ResolvedAbility>` before this change, so `assert_eq!(ability,
//!   control)` pins the newly-boxed fields against a spelling whose wire format
//!   is known-good. That catches an *asymmetry* — one field picking up a rename,
//!   a `flatten`, or a different container attribute than its long-boxed
//!   sibling. It cannot catch a *uniform* regression: if `Box<T>` stopped being
//!   transparent, the control would grow the same wrapper as the fields under
//!   test and the comparison would still hold. So the control comparison alone
//!   would be vacuous; it earns its place only alongside the key-path
//!   assertions, which is why both are present.
//!
//! Do not delete the key-path assertions on the grounds that the `assert_eq!`
//! against the control "already covers it". It does not.

use engine::game::scenario::{P0, P1};
use engine::game::triggers::PendingTrigger;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::game_state::{
    GameState, PendingCast, PendingDiscardForCostResume, PersistedGameState, StackEntry,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

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
    state.pending_trigger = Some(Box::new(PendingTrigger::ordinary(
        SOURCE,
        P0,
        None,
        Box::new(damage_ability()),
        9,
    )));
    // Populated so the `#[serde(skip)]` assertion in
    // `boxing_introduces_no_wrapper_level_in_the_wire_shape` discriminates. With
    // this field left `None`, that assertion could only distinguish `skip` from
    // serialize-as-`null`, and would pass unchanged if the attribute were
    // swapped to `skip_serializing_if = "Option::is_none"` — under which a
    // *populated* field would cross the wire. Populated, the assertion fails for
    // any attribute that does not actually suppress the field.
    //
    // Safe for `boxed_abilities_round_trip_through_serde`: `GameState`'s manual
    // `PartialEq` deliberately excludes this field, so an intentionally-dropped
    // skipped field cannot make the round-trip equality assertion red.
    state.pending_discard_for_cost = Some(Box::new(PendingDiscardForCostResume {
        player: P0,
        pending: PendingCast::new(
            ObjectId(704),
            CardId(1),
            damage_ability(),
            ManaCost::default(),
        ),
        chosen: vec![ObjectId(705)],
        paused_at_index: 0,
    }));
    state
}

#[test]
fn boxed_abilities_round_trip_through_serde() {
    let mut value = serde_json::to_value(populated_state()).expect("populated state serializes");
    value["stack_trigger_firings"] = serde_json::json!({ "703": "Ordinary" });
    value["pending_trigger_firing"] = serde_json::json!("Ordinary");
    let state: GameState = serde_json::from_value(value).expect("canonical state deserializes");

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

    let json = serde_json::to_string(&state).expect("canonical state serializes");
    let mut restored: GameState = serde_json::from_str(&json).expect("and deserializes");
    restored.rng = state.rng.clone(); // skipped by serde; not under test here

    assert_eq!(
        state, restored,
        "a state with populated boxed abilities must survive a serde round trip"
    );
}

#[test]
fn boxing_introduces_no_wrapper_level_in_the_wire_shape() {
    let state = populated_state();

    // Reach-guard for the `pending_discard_for_cost` assertion at the end of
    // this test, asserted on the *same* instance that is serialized below (not
    // on a second `populated_state()`), so it is evidence about this value
    // rather than about the fixture function in general.
    assert!(
        state.pending_discard_for_cost.is_some(),
        "reach-guard: the fixture must populate pending_discard_for_cost, or the \
         #[serde(skip)] assertion at the end of this test passes vacuously"
    );

    let value = serde_json::to_value(state).expect("state serializes");
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
    // promoted it onto the wire. The reach-guard at the top of this test is what
    // makes this discriminating: the serialized value really did carry a
    // populated field, so its absence here is evidence the attribute suppressed
    // it, not evidence that there was nothing to suppress.
    //
    // Scope honestly: `PendingDiscardForCostResume` is not `Serialize`
    // (`types/game_state.rs`, derives `Debug, Clone, PartialEq, Eq` only), so
    // this field cannot reach the wire until someone first adds the derives —
    // the compiler forbids the one-step regression, and only a deliberate
    // two-step change can reach it. What this assertion pins is the outcome if
    // they ever do. By contrast the sibling `pending_cost_move_resume` *is*
    // `Serialize` and uses `skip_serializing_if = "Option::is_none"`, under
    // which a populated field does cross the wire — a contrast that shows the
    // two attributes differ, not a live risk on this field.
    assert!(
        value.get("pending_discard_for_cost").is_none(),
        "pending_discard_for_cost is #[serde(skip)] and must stay off the wire, \
         but the fixture populates it and it appeared in {value}"
    );
}

/// `GameState::resolving_stack_entry: Option<StackEntry>` shrank 5,336 -> 344 B
/// through this change, because `StackEntryKind::Spell.ability` was boxed
/// underneath it. It is the field the *persisted* path depends on: CR 707.10 —
/// the Chain cycle ("you may copy this spell") defers its copy past an
/// `OptionalEffectChoice`, so a server game saved with that prompt pending and
/// later reloaded must still find the popped entry, or the accepted copy is
/// silently dropped.
///
/// The two tests above are not sufficient for that. They round-trip `GameState`
/// through `serde_json` directly, whereas persistence goes through
/// `PersistedGameState`, whose `Serialize`/`Deserialize` are **hand-written**:
/// both branches funnel through `ResolutionStateWire::to_value`, which runs
/// `canonicalize_legacy_resolution_state` + `frames.validate(&waiting_for)`
/// before emitting, and `PersistedGameState::deserialize` dispatches on a
/// top-level `"state"` key. None of that is exercised by a bare `GameState`
/// round trip, so this test covers the persisted seam on its own terms.
fn state_with_resolving_stack_entry() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(704),
        source_id: SOURCE,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(Box::new(damage_ability())),
            casting_variant: Default::default(),
            actual_mana_spent: 2,
        },
    });
    state
}

#[test]
fn persisted_round_trip_preserves_the_boxed_resolving_stack_entry() {
    let state = state_with_resolving_stack_entry();

    // Reach-guard: the fixture reaches the branch under test. Without a
    // populated `resolving_stack_entry` this whole test is a no-op that passes
    // for any layout, because the field is `skip_serializing_if =
    // "Option::is_none"` and would never appear on the wire at all.
    let entry = state
        .resolving_stack_entry
        .as_ref()
        .expect("reach-guard: resolving_stack_entry is populated");
    assert!(
        matches!(
            &entry.kind,
            StackEntryKind::Spell {
                ability: Some(_),
                ..
            }
        ),
        "reach-guard: the resolving entry carries a populated boxed Spell ability, got {:?}",
        entry.kind
    );

    let json = serde_json::to_string(&PersistedGameState::capture(state.clone()))
        .expect("a trusted persisted snapshot serializes");

    // The discriminator that makes this more than a `GameState` round trip: the
    // boxed field must survive the hand-written `PersistedGameState` codec, not
    // just the derived `GameState` one.
    let restored = serde_json::from_str::<PersistedGameState>(&json)
        .expect("and deserializes back through the persisted codec")
        .into_game_state();

    let restored_entry = restored
        .resolving_stack_entry
        .as_ref()
        .expect("resolving_stack_entry must survive the persisted round trip");
    assert_eq!(
        restored_entry.ability().map(|ability| &ability.effect),
        state
            .resolving_stack_entry
            .as_ref()
            .and_then(StackEntry::ability)
            .map(|ability| &ability.effect),
        "the boxed ability inside resolving_stack_entry must survive persistence \
         with its effect intact"
    );

    // And it must be flat on the persisted wire: no `Box` wrapper level at the
    // key path a persisted save is actually read back from.
    let value: serde_json::Value = serde_json::from_str(&json).expect("persisted JSON parses");
    let persisted_ability = &value["state"]["resolving_stack_entry"]["kind"]["data"]["ability"];
    assert!(
        persisted_ability.get("effect").is_some(),
        "resolving_stack_entry's boxed ability must serialize as a bare \
         ResolvedAbility object on the persisted wire, got {persisted_ability}"
    );
}

#[test]
fn persisted_restore_migrates_legacy_jeskas_will_mana_target_role() {
    let mut state = state_with_resolving_stack_entry();
    let source = create_object(
        &mut state,
        CardId(2),
        P0,
        "Jeska's Will".to_string(),
        Zone::Hand,
    );
    let entry = state
        .resolving_stack_entry
        .as_mut()
        .expect("reach-guard: resolving stack entry is populated");
    entry.source_id = source;
    let ability = entry
        .ability_mut()
        .expect("reach-guard: resolving spell has an ability");
    ability.source_id = source;

    let mut persisted = serde_json::to_value(PersistedGameState::capture(state))
        .expect("a current persisted snapshot serializes");
    let effect =
        &mut persisted["state"]["resolving_stack_entry"]["kind"]["data"]["ability"]["effect"];
    *effect = serde_json::json!({
        "type": "Mana",
        "produced": {
            "type": "AnyOneColor",
            "count": {
                "type": "Ref",
                "qty": { "type": "TargetZoneCardCount", "zone": "Hand" }
            },
            "color_options": ["Red"]
        },
        "target": {
            "type": "Typed",
            "type_filters": [],
            "controller": "Opponent",
            "properties": []
        }
    });

    let target =
        &persisted["state"]["resolving_stack_entry"]["kind"]["data"]["ability"]["effect"]["target"];
    assert!(
        target.get("role").is_none(),
        "reach-guard: the fixture must carry the pre-ManaTargetRole target encoding"
    );

    let restored = serde_json::from_value::<PersistedGameState>(persisted)
        .expect("legacy Jeska's Will snapshot restores through the persisted codec")
        .into_game_state();
    let reserialized = serde_json::to_value(PersistedGameState::capture(restored))
        .expect("the migrated state reserializes");
    assert_eq!(
        reserialized["state"]["resolving_stack_entry"]["kind"]["data"]["ability"]["effect"]
            ["target"],
        serde_json::json!({
            "role": "CountSource",
            "count_source": {
                "type": "Typed",
                "type_filters": [],
                "controller": "Opponent",
                "properties": []
            }
        }),
        "Jeska's Will target must restore as its Oracle-defined count source"
    );
}
