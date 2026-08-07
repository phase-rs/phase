//! PR #6056 blocker — a FORCED retarget (Redirect/Spellskite-style "change a
//! target", CR 115.7b) of a multi-role mana ability must change only ONE target
//! slot and PRESERVE the other declared slot in place.
//!
//! A `ManaTargetRole::Both` mana ability declares two independent instances of
//! the word "target" (CR 601.2c): a RECIPIENT slot (whose pool receives the
//! mana, CR 106.4) and a COUNT-SOURCE slot (the player a production count reads,
//! CR 115.1). Both surface real cast-time target slots.
//!
//! The pre-fix forced branch did `stack_ability.targets = vec![new_target]`,
//! which COLLAPSED the two-slot target list to a single element — deleting the
//! untouched count-source slot, a deletion CR 115.7a/b forbid ("the OTHER
//! targets stay unchanged"). This drives the real `ChangeTargets` resolution
//! (`stack::resolve_top`) with a forced target and asserts the surviving list is
//! length 2 with the changed slot updated and the untouched slot preserved in
//! its original position.

use engine::game::stack::resolve_top;
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, ManaProduction, ManaTargetRole, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastingVariant, GameState, RetargetScope, StackEntry, StackEntryKind,
};
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// CR 115.7a + CR 115.7b + CR 601.2c: forced retarget of a `Both` mana ability
/// changes the recipient slot to P3 and leaves the count-source slot (P2) in
/// place. The list must stay length 2.
///
/// Revert-failing assertion: `count_source_slot == P2` AND `targets.len() == 2`.
/// The pre-fix `stack_ability.targets = vec![new_target]` collapses the list to
/// `[P3]` (length 1), dropping P2 entirely — both assertions fail.
///
/// Positive reach guard (so the test is not vacuous): the recipient slot
/// actually CHANGED from P1 to P3, proving the forced retarget ran past the
/// legality gate and reached the multi-slot assignment.
#[test]
fn forced_retarget_of_both_mana_preserves_untouched_count_source_slot() {
    // Four seats so recipient (P1), count source (P2), and the forced new
    // recipient (P3) are three distinct legal players, with P0 controlling the
    // retargeter.
    let mut state = GameState::new(FormatConfig::standard(), 4, 42);

    // A multi-role `Both` mana spell on the stack: recipient = P1, count source
    // = P2, both `TargetFilter::Player` (two surfaced slots => `mana_multi_role`
    // is `Some`). Targets are declared recipient-first, matching
    // `surfaced_filters()` order.
    let mana_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Both-Role Mana".to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&mana_id).unwrap();
        obj.card_types.core_types = vec![CoreType::Sorcery];
    }
    let mana_ability = ResolvedAbility::new(
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(ManaTargetRole::Both {
                recipient: TargetFilter::Player,
                count_source: TargetFilter::Player,
            }),
        },
        vec![TargetRef::Player(P1), TargetRef::Player(P2)],
        mana_id,
        PlayerId(0),
    );
    state.stack.push_back(StackEntry {
        id: mana_id,
        source_id: mana_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(Box::new(mana_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // A forced retarget on top of the stack, targeting the mana spell, forced to
    // P3 specifically (`SpecificPlayer`). `StackSpell` keeps resolution-time
    // re-validation (CR 608.2b) from fizzling the stack target.
    let retarget_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Forced Redirect".to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&retarget_id).unwrap();
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    let retarget_ability = ResolvedAbility::new(
        Effect::ChangeTargets {
            target: TargetFilter::StackSpell,
            scope: RetargetScope::Single,
            forced_to: Some(TargetFilter::SpecificPlayer { id: P3 }),
        },
        vec![TargetRef::Object(mana_id)],
        retarget_id,
        PlayerId(0),
    );
    state.stack.push_back(StackEntry {
        id: retarget_id,
        source_id: retarget_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(2),
            ability: Some(Box::new(retarget_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Drive the REAL resolution of the forced ChangeTargets.
    let mut events: Vec<GameEvent> = Vec::new();
    resolve_top(&mut state, &mut events);

    let mana_targets = state
        .stack
        .iter()
        .find(|e| e.id == mana_id)
        .and_then(|e| e.ability())
        .map(|a| a.targets.clone())
        .expect("mana spell must remain on the stack with its targets");

    // Revert-failing assertion #1: both slots survive — length is still 2.
    // Pre-fix `vec![new_target]` collapses this to 1.
    assert_eq!(
        mana_targets.len(),
        2,
        "forced retarget must keep BOTH declared target slots, got {mana_targets:?}"
    );
    // Positive reach guard: the recipient slot (index 0) actually changed to P3.
    assert_eq!(
        mana_targets[0],
        TargetRef::Player(P3),
        "recipient slot must be retargeted to P3, got {mana_targets:?}"
    );
    // Revert-failing assertion #2: the untouched count-source slot (index 1) is
    // preserved as P2 in its original position. Pre-fix this slot is gone.
    assert_eq!(
        mana_targets[1],
        TargetRef::Player(P2),
        "count-source slot must be preserved as P2, got {mana_targets:?}"
    );
}

/// CR 115.7a (Matt #6056 follow-up): with OVERLAPPING role filters and current
/// targets [P1, P2], forcing P1 must change the COUNT-SOURCE slot (P2 -> P1)
/// rather than no-op on the recipient slot that already holds P1. "Each target
/// can be changed only to ANOTHER legal target" -- a slot already holding the
/// candidate is not a change, so slot selection must skip it and use the slot
/// that can genuinely change. Result: [P1, P1].
///
/// Revert-failing: the first-match-only predicate (no "current != new" guard)
/// selects slot 0 (P1 already accepts P1), writes P1 back, and leaves [P1, P2]
/// -- the available legal change on slot 1 is never made.
#[test]
fn forced_retarget_prefers_the_slot_that_actually_changes() {
    let mut state = GameState::new(FormatConfig::standard(), 4, 42);

    let mana_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Both-Role Mana".to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&mana_id).unwrap();
        obj.card_types.core_types = vec![CoreType::Sorcery];
    }
    let mana_ability = ResolvedAbility::new(
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            // Overlapping filters: both accept any player, so the candidate P1
            // is legal for BOTH slots -- only the "must change" rule disambiguates.
            target: Some(ManaTargetRole::Both {
                recipient: TargetFilter::Player,
                count_source: TargetFilter::Player,
            }),
        },
        vec![TargetRef::Player(P1), TargetRef::Player(P2)],
        mana_id,
        PlayerId(0),
    );
    state.stack.push_back(StackEntry {
        id: mana_id,
        source_id: mana_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(Box::new(mana_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let retarget_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Forced Redirect".to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&retarget_id).unwrap();
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    let retarget_ability = ResolvedAbility::new(
        Effect::ChangeTargets {
            target: TargetFilter::StackSpell,
            scope: RetargetScope::Single,
            // Force to P1 -- a player ALREADY held by slot 0 (the recipient).
            forced_to: Some(TargetFilter::SpecificPlayer { id: P1 }),
        },
        vec![TargetRef::Object(mana_id)],
        retarget_id,
        PlayerId(0),
    );
    state.stack.push_back(StackEntry {
        id: retarget_id,
        source_id: retarget_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(2),
            ability: Some(Box::new(retarget_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let mut events: Vec<GameEvent> = Vec::new();
    resolve_top(&mut state, &mut events);

    let mana_targets = state
        .stack
        .iter()
        .find(|e| e.id == mana_id)
        .and_then(|e| e.ability())
        .map(|a| a.targets.clone())
        .expect("mana spell must remain on the stack with its targets");

    assert_eq!(
        mana_targets.len(),
        2,
        "both slots must survive, got {mana_targets:?}"
    );
    // Recipient slot 0 was already P1 and must be left untouched.
    assert_eq!(
        mana_targets[0],
        TargetRef::Player(P1),
        "recipient slot already held P1 and must be unchanged, got {mana_targets:?}"
    );
    // Revert-failing: count-source slot 1 must have changed P2 -> P1. The
    // first-match predicate leaves this as P2.
    assert_eq!(
        mana_targets[1],
        TargetRef::Player(P1),
        "count-source slot must change P2 -> P1 (the only real change), got {mana_targets:?}"
    );
}
