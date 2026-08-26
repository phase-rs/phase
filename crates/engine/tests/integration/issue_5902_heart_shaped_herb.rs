//! Issue #5902: Heart-Shaped Herb must prevent 1 damage from each opponent-
//! controlled source dealt to you, and must not affect damage from your own
//! sources. This is a discriminating engine scenario driving the real
//! `deal_damage::resolve` pipeline, not just parsing.
//!
//! Before this fix, "prevent N of that damage" (no "all" / "all but" / "the
//! next" qualifier) matched none of `parse_damage_prevention_replacement`'s
//! amount branches, so the whole static ability failed to parse into a
//! `ReplacementDefinition` at all — the reported symptom was that Heart-
//! Shaped Herb "isn't affecting it at all".
//!
//! Per the maintainer's CR review (add-engine-variant Stage 1 =
//! `REFUSE_WITH_EXISTING_SLOT`), the fix reuses the existing shared
//! per-event damage-subtraction authority — the `Minus` applier arm — under
//! its typed prevention provenance `DamageModification::PreventionMinus
//! { value: N }` on a `DamageDone` replacement, the same continuous,
//! non-consumed representation CR 702.64 Absorb already synthesizes
//! (`database::synthesis::build_absorb_replacement`) — instead of a new
//! `PreventionAmount` variant. Only the prevention provenance emits the
//! `DamagePrevented` bookkeeping the prevention shields emit; plain
//! arithmetic `Minus` (Benevolent Unicorn) does not.
//!
//! CR 615.1a (prevention shield) + CR 702.64b (continuous, non-depleting) +
//! CR 609.7b (controller-scoped source restriction).

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    DamageModification, Effect, GameRestriction, QuantityExpr, ResolvedAbility, RestrictionExpiry,
    ShieldKind, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::player::PlayerId;

const HEART_SHAPED_HERB: &str =
    "If a source an opponent controls would deal damage to you, prevent 1 of that damage.";

fn damage_to_player_ability(
    source_id: engine::types::identifiers::ObjectId,
    controller: PlayerId,
    target: TargetRef,
    amount: i32,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        controller,
    )
}

#[test]
fn heart_shaped_herb_prevents_one_from_opponent_sources_not_own_and_never_depletes() {
    let mut scenario = GameScenario::new();
    // Built as a 0/1 creature so it survives build-time SBAs, then converted
    // into a plain Artifact below — same convention as the Panther Habit
    // (issue #5246) prevention test.
    let herb = scenario
        .add_creature_from_oracle(P0, "Heart-Shaped Herb", 0, 1, HEART_SHAPED_HERB)
        .id();
    let opponent_source_a = scenario.add_creature(P1, "Opponent Source A", 3, 3).id();
    let opponent_source_b = scenario.add_creature(P1, "Opponent Source B", 3, 3).id();
    let own_source = scenario.add_creature(P0, "Own Source", 3, 3).id();
    let mut runner = scenario.build();

    {
        let obj = runner.state_mut().objects.get_mut(&herb).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec![];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }

    // CR 615.1a + CR 702.64: the shield installs as a continuous
    // `DamageModification::PreventionMinus { value: 1 }` `DamageDone`
    // replacement — the shared per-event subtraction authority under its typed
    // prevention provenance, NOT a bespoke prevention-amount variant — with
    // `shield_kind` left as the default `None`.
    let repl = &runner.state().objects[&herb].replacement_definitions[0];
    assert_eq!(
        repl.damage_modification,
        Some(DamageModification::PreventionMinus { value: 1 }),
        "Heart-Shaped Herb must install a continuous PreventionMinus(1) damage replacement, got {:?}",
        repl.damage_modification
    );
    assert_eq!(
        repl.shield_kind,
        ShieldKind::None,
        "the Minus representation must not also carry a prevention shield_kind, got {:?}",
        repl.shield_kind
    );

    // Damage from an OPPONENT-controlled source is reduced by 1.
    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_to_player_ability(opponent_source_a, P1, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("opponent damage to P0 resolves");
    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "3 damage from an opponent-controlled source must be reduced to 2"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 1, .. })),
        "the shared Minus authority must emit DamagePrevented for the 1 point it prevents"
    );

    // The shield must NOT be exhausted — a SECOND opponent-source damage
    // event must also be reduced by 1 (CR 702.64b: continuous, non-consumed).
    // A depleting shield would only ever prevent 1 damage total.
    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_to_player_ability(opponent_source_b, P1, TargetRef::Player(P0), 4),
        &mut events,
    )
    .expect("second opponent damage to P0 resolves");
    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "shield must re-fire on a second opponent-source event, not be exhausted by the first"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 1, .. })),
        "must prevent exactly 1 of the second opponent-source damage event"
    );

    // Damage from the shield controller's OWN source is not affected at all
    // (CR 609.7b: the shield is scoped to sources an opponent controls).
    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_to_player_ability(own_source, P0, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("self-inflicted damage to P0 resolves");
    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "damage from a source P0 controls must not be prevented by P0's own shield"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "no prevention event should fire for a source the shield's controller controls"
    );

    // Damage that can't be prevented must bypass the typed
    // `PreventionMinus` replacement while still allowing ordinary damage.
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::DamagePreventionDisabled {
            source: herb,
            expiry: RestrictionExpiry::EndOfTurn,
            scope: None,
        });
    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_to_player_ability(opponent_source_a, P1, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("unpreventable opponent damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "damage that can't be prevented must not be reduced by PreventionMinus"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "unpreventable damage must emit no DamagePrevented event"
    );
}

/// CR 614.1a vs CR 615: negative provenance regression (maintainer review on
/// PR #6307). Benevolent Unicorn's "it deals that much damage minus 1 instead"
/// is a plain ARITHMETIC damage replacement — `DamageModification::Minus`, the
/// same shared subtraction arm — but it is NOT prevention: no damage is
/// "prevented" in the CR 615 sense, so reducing an event must emit NO
/// `DamagePrevented` (which would wrongly feed prevention-triggered abilities)
/// and must NOT stamp the CR 615.5 "damage prevented this way" continuation
/// handoff. Before the typed-provenance fix, every `Minus` was classified as
/// prevention and this card emitted phantom prevention bookkeeping.
#[test]
fn benevolent_unicorn_arithmetic_minus_reduces_without_prevention_bookkeeping() {
    let mut scenario = GameScenario::new();
    let unicorn = scenario
        .add_creature_from_oracle(
            P0,
            "Benevolent Unicorn",
            1,
            2,
            "If a spell would deal damage to a permanent or player, it deals that much damage minus 1 to that permanent or player instead.",
        )
        .id();
    let source = scenario.add_creature(P1, "Damage Source", 3, 3).id();
    let mut runner = scenario.build();

    // The static parses to the ARITHMETIC provenance of the shared subtraction:
    // `Minus`, never `PreventionMinus`.
    let repl = &runner.state().objects[&unicorn].replacement_definitions[0];
    assert_eq!(
        repl.damage_modification,
        Some(DamageModification::Minus { value: 1 }),
        "'that much damage minus 1' must stay plain arithmetic Minus(1), got {:?}",
        repl.damage_modification
    );
    assert_eq!(repl.shield_kind, ShieldKind::None);

    let p0_life_before = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_to_player_ability(source, P1, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("damage to P0 resolves");
    assert_eq!(
        runner.life(P0),
        p0_life_before - 2,
        "the arithmetic replacement must still reduce 3 damage to 2"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "arithmetic 'minus 1' prevents nothing — it must emit NO DamagePrevented \
         and satisfy no prevention-triggered ability"
    );
    assert_eq!(
        runner.state().last_effect_count,
        None,
        "arithmetic 'minus 1' must not stamp the CR 615.5 prevented-amount handoff"
    );
}
