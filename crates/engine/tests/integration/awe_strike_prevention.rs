//! Awe Strike — one-shot target-source prevention, driven through the real
//! cast pipeline (GameScenario + GameRunner::cast(...).resolve()).
//!
//! Verbatim Oracle text under test (Scryfall):
//!   "The next time target creature would deal damage this turn, prevent that
//!    damage. You gain life equal to the damage prevented this way."
//!
//! Defects this guards against (backlog class 11):
//!   1. The target creature was dropped — the shield prevented EVERY source's
//!      damage instead of only the chosen creature's.
//!   2. The shield never consumed — "the next time" is a single opportunity
//!      (CR 615.3), so later damage events must go through.
//!   3. The "gain life equal to the damage prevented this way" rider was
//!      dropped as an independent sibling instead of firing off the shield.
//!
//! CR 614.1a + CR 615.1a + CR 615.3 + CR 609.7a + CR 609.7b + CR 514.2 +
//! CR 115.1.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    Effect, QuantityExpr, ResolvedAbility, ShieldKind, TargetFilter, TargetRef,
};
use engine::types::events::GameEvent;
use engine::types::phase::Phase;

const AWE_STRIKE: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";

fn damage_ability(
    source_id: engine::types::identifiers::ObjectId,
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
        P1,
    )
}

/// CR 615.1a + CR 615.3: The first damage event from the chosen creature is
/// prevented and the rider gains life equal to the prevented amount exactly
/// once; a second event goes through (single opportunity, CR 615.3).
#[test]
fn awe_strike_prevents_first_source_damage_once_then_lets_second_through() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b1 = scenario.add_creature(P1, "B1", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .id();

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let outcome = runner.cast(spell).target_objects(&[b1]).resolve();
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "casting Awe Strike itself must not change P0's life"
    );

    // First event: 3 damage from b1 to P0 → prevented, +3 life (CR 615.5).
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(b1, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("b1's first damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_before + 3,
        "first b1 damage prevented AND P0 gains life equal to the prevented damage"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 3, .. })),
        "the first damage event must emit DamagePrevented(3)"
    );

    // Second event: 3 damage from b1 to P0 → goes through (no shield left).
    let p0_now = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(b1, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("b1's second damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_now - 3,
        "second b1 damage must go through — the one-shot shield was consumed"
    );
}

/// CR 120.8 + CR 609.7b (fix round 2, review #7334): a 0-damage event must
/// not consume the one-shot shield and must not fire the "damage prevented
/// this way" rider. A 0-damage source deals no damage at all (CR 120.8) — the
/// prevention has no event to replace — and a shield that prevents no damage
/// is not used up (CR 609.7b). This test drives the replacement pipeline with
/// a genuine 0-amount `ProposedEvent::Damage` from the chosen creature (the
/// public `replace_event` seam, reached by `deal_damage`'s gate on every
/// production path), then proves the shield still stops a later 3-damage event
/// with the rider firing exactly once (+3, and the 0-damage pass produced no
/// life gain and no `DamagePrevented` event).
#[test]
fn awe_strike_zero_damage_event_does_not_consume_shield_or_fire_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b1 = scenario.add_creature(P1, "B1", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .id();

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let _outcome = runner.cast(spell).target_objects(&[b1]).resolve();
    let shield = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| r.shield_kind == ShieldKind::PreventionOneShot)
        .expect("the one-shot shield must be installed");
    assert!(
        !shield.is_consumed,
        "reach guard: the shield starts unconsumed"
    );

    // Zero-damage event from the chosen creature (CR 120.8: deals no damage).
    let mut events = Vec::new();
    let result = engine::game::replacement::replace_event(
        runner.state_mut(),
        engine::types::proposed_event::ProposedEvent::Damage {
            source_id: b1,
            target: TargetRef::Player(P0),
            amount: 0,
            is_combat: false,
            applied: std::collections::HashSet::new(),
        },
        &mut events,
    );
    assert!(
        matches!(
            result,
            engine::game::replacement::ReplacementResult::Execute(_)
        ),
        "a 0-damage event must pass through unmodified, got {result:?}"
    );
    assert_eq!(
        runner.life(P0),
        p0_before,
        "a 0-damage event must not change life (no prevention, no rider gain)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "a 0-damage event must not emit DamagePrevented"
    );
    let shield = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| r.shield_kind == ShieldKind::PreventionOneShot)
        .expect("the shield must still exist after the 0-damage event");
    assert!(
        !shield.is_consumed,
        "CR 609.7b: a shield that prevented no damage must not be used up"
    );

    // The shield survives: the next nonzero event from the chosen creature is
    // still prevented and the rider fires exactly once (+3).
    let p0_now = runner.life(P0);
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(b1, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("b1's 3-damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_now + 3,
        "the surviving shield must still prevent the later 3-damage event and \
         gain life equal to the damage prevented this way"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { amount: 3, .. })),
        "the 3-damage event must emit DamagePrevented(3)"
    );
    let shield = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| r.shield_kind == ShieldKind::PreventionOneShot);
    assert!(
        shield.is_none() || shield.is_some_and(|r| r.is_consumed),
        "after the 3-damage prevention the one-shot shield must be consumed (CR 615.3)"
    );
}

/// CR 609.7a + CR 609.7b: the shield binds to the CHOSEN creature only — a
/// different source's damage goes through untouched, and the shield survives.
#[test]
fn awe_strike_shield_binds_to_chosen_creature_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b1 = scenario.add_creature(P1, "B1", 3, 3).id();
    let b2 = scenario.add_creature(P1, "B2", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .id();

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let _outcome = runner.cast(spell).target_objects(&[b1]).resolve();

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(b2, TargetRef::Player(P0), 3),
        &mut events,
    )
    .expect("b2's damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_before - 3,
        "a non-chosen creature's damage must NOT be prevented"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "no prevention event may fire for a non-chosen source"
    );
}

/// CR 510.2 + CR 615.5 + CR 615.3: within a simultaneous combat-damage batch
/// (driven through the real combat step), the one-shot shield prevents the
/// single matching event and the rider fires exactly once (+3, not +6).
#[test]
fn awe_strike_prevents_combat_damage_batch_with_single_rider_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 controls the attacker and casts Awe Strike (P0 is the active player
    // with priority in PreCombatMain); the attacker attacks P1.
    let b1 = scenario.add_creature(P0, "B1", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .id();

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let _outcome = runner.cast(spell).target_objects(&[b1]).resolve();
    // Sanity: the shield is installed in the pending registry.
    assert!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.shield_kind.is_shield()),
        "the one-shot shield must be installed before combat"
    );

    // Drive b1 through a real combat step: it attacks P1 unblocked, dealing 3
    // combat damage in a simultaneous batch (CR 510.2). Manual driver —
    // `advance_to_combat` auto-passes the DeclareAttackers action (declaring
    // no attackers), so pass priority manually until the prompt surfaces.
    let mut attacked = false;
    let mut blocked = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            engine::types::game_state::WaitingFor::Priority { .. } => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
            engine::types::game_state::WaitingFor::DeclareAttackers { .. } if !attacked => {
                attacked = true;
                if runner
                    .declare_attackers(&[(b1, engine::game::combat::AttackTarget::Player(P1))])
                    .is_err()
                {
                    break;
                }
            }
            engine::types::game_state::WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            engine::types::game_state::WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            engine::types::game_state::WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            engine::types::game_state::WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(engine::types::actions::GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_before + 3,
        "the combat damage is prevented and the rider gains +3 exactly once \
         (a double fire would gain +6); got {}",
        runner.life(P0)
    );
}

/// CR 609.7 + CR 614.1a: the shield is source-scoped, so the chosen creature's
/// damage to a CREATURE recipient is prevented too.
#[test]
fn awe_strike_prevents_chosen_sources_damage_to_creature_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b1 = scenario.add_creature(P1, "B1", 3, 3).id();
    let victim = scenario.add_creature(P0, "Victim", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .id();

    let mut runner = scenario.build();
    let _outcome = runner.cast(spell).target_objects(&[b1]).resolve();

    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(b1, TargetRef::Object(victim), 2),
        &mut events,
    )
    .expect("b1's damage to the creature resolves");
    assert_eq!(
        runner.state().objects[&victim].damage_marked,
        0,
        "the chosen creature's damage to a creature recipient must be prevented"
    );
}

/// CR 115.1 + CR 601.2c + CR 609.7a: the cast surfaces exactly one target
/// slot (the chosen creature) and the installed shield's resolved source
/// filter is `And { [SpecificObject(b1), Typed(creature)] }` — a source-scoped
/// shield, not a blanket prevent-all, and not hosted on the creature.
#[test]
fn awe_strike_cast_surfaces_single_creature_slot_and_source_scoped_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b1 = scenario.add_creature(P1, "B1", 3, 3).id();
    let b2 = scenario.add_creature(P1, "B2", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[b1]).resolve();

    let state = outcome.state();
    let shield = state
        .pending_damage_replacements
        .iter()
        .find(|r| r.shield_kind.is_shield())
        .expect("the one-shot shield must be installed in the pending registry");
    assert_eq!(
        shield.shield_kind,
        ShieldKind::PreventionOneShot,
        "the shield must be the one-shot prevention kind"
    );
    assert_eq!(
        shield.damage_source_filter,
        Some(TargetFilter::And {
            filters: vec![
                TargetFilter::SpecificObject { id: b1 },
                TargetFilter::Typed(engine::types::ability::TypedFilter::creature()),
            ],
        }),
        "the shield must bind to the chosen creature (CR 609.7a) with the typed \
         CR 609.7b recheck leaf — not a blanket prevent-all"
    );
    // Not hosted on either creature as a recipient.
    assert!(
        !state.objects[&b1]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|r| r.shield_kind.is_shield()),
        "the source-scoped shield must not be hosted on the chosen creature as a recipient"
    );
    assert!(
        !state.objects[&b2]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|r| r.shield_kind.is_shield()),
        "the shield must not be hosted on the unchosen creature"
    );
}

/// CR 615.5: Dazzling Reflection — "You gain life equal to target creature's
/// power. The next time that creature would deal damage this turn, prevent
/// that damage." The rider gains the target creature's power immediately, and
/// the "that creature" one-shot prevention binds to the chosen creature.
#[test]
fn dazzling_reflection_target_power_gain_and_that_creature_prevent() {
    const DAZZLING_REFLECTION: &str = "You gain life equal to target creature's power. The next time that creature would deal damage this turn, prevent that damage.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b1 = scenario.add_creature(P1, "B1", 5, 5).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dazzling Reflection", true, DAZZLING_REFLECTION)
        .id();

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let _outcome = runner.cast(spell).target_objects(&[b1]).resolve();

    // The first instruction gains life equal to b1's power.
    assert_eq!(
        runner.life(P0),
        p0_before + 5,
        "Dazzling Reflection's own gain-life must equal the target creature's power (5)"
    );

    // The one-shot prevention still protects the chosen creature.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(b1, TargetRef::Player(P0), 2),
        &mut events,
    )
    .expect("b1's damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_before + 5,
        "the 'that creature' one-shot prevention must still prevent b1's damage"
    );
}

/// CR 609.7 + CR 615.3: a Dromoka's Command source-scoped prevention shield
/// (Typed(instant|sorcery) leaf) must NOT become one-shot — its
/// `Prevention { All }` shield stays continuous.
///
/// Positioning note (discriminator-precision guard, one-sided): this test is
/// the NEGATIVE side of the exact-shape discriminator
/// (`is_oneshot_target_source_prevent_shape`) — it proves the shape predicate
/// REJECTS a `Typed(instant|sorcery)` leaf, so a too-wide discriminator
/// (any `ParentTargetSlot` + any creature-containing `Typed`) fails here.
/// It does NOT by itself prove the positive side: if the whole one-shot
/// machinery were reverted, this test still passes (a missing discriminator
/// trivially leaves `Prevention { All }`). The positive side is pinned by
/// `awe_strike_cast_surfaces_single_creature_slot_and_source_scoped_shield`
/// and `awe_strike_prevents_first_source_damage_once_then_lets_second_through`
/// (both fail on revert — verified). A positive in-test assertion that the
/// shared predicate accepts the Awe Strike shape is added below to keep the
/// two sides of the same predicate in one place.
#[test]
fn dromokas_command_source_scoped_shield_is_not_consumed() {
    const DROMOKAS: &str = "Choose two —\n\
        • Prevent all damage target instant or sorcery spell would deal this turn.\n\
        • Target player sacrifices an enchantment.\n\
        • Put a +1/+1 counter on target creature.\n\
        • Target creature you control fights target creature you don't control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let my_creature = scenario.add_creature(P0, "Bear", 2, 2).id();
    let instant = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Damage Instant",
            true,
            "This spell deals 3 damage to target player.",
        )
        .id();
    let dromoka = scenario
        .add_spell_to_hand_from_oracle(P0, "Dromoka's Command", true, DROMOKAS)
        .id();

    let mut runner = scenario.build();
    // Cast the instant, answer its player target, leave it on the stack.
    let instant_card = runner.state().objects[&instant].card_id;
    runner
        .act(engine::types::actions::GameAction::CastSpell {
            object_id: instant,
            card_id: instant_card,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast the damage instant");
    if let engine::types::game_state::WaitingFor::TargetSelection { .. } =
        runner.state().waiting_for.clone()
    {
        let _ = runner.act(engine::types::actions::GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        });
    }
    let _outcome = runner
        .cast(dromoka)
        .modes(&[0, 2])
        .target_objects(&[instant, my_creature])
        .resolve();

    let shield = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| {
            matches!(
                &r.damage_source_filter,
                Some(TargetFilter::And { filters })
                    if filters
                        .iter()
                        .any(|f| matches!(f, TargetFilter::SpecificObject { .. }))
            )
        })
        .expect("the source-scoped shield must exist");
    assert_eq!(
        shield.shield_kind,
        ShieldKind::Prevention {
            amount: engine::types::ability::PreventionAmount::All
        },
        "Dromoka's Command must install a continuous Prevention All shield, not one-shot"
    );
    assert!(
        !shield.consume_on_apply,
        "Dromoka's Command shield must not be consume-on-apply"
    );
    // Positive side of the same predicate, in the same test: the shared
    // exact-shape discriminator ACCEPTS the Awe Strike source-filter shape
    // (And{[ParentTargetSlot{0}, Typed(creature)]}) — proving the negative
    // assertions above exercise a live predicate that does discriminate.
    assert!(
        engine::types::ability::is_oneshot_target_source_prevent_shape(&TargetFilter::And {
            filters: vec![
                TargetFilter::ParentTargetSlot { index: 0 },
                TargetFilter::Typed(engine::types::ability::TypedFilter::creature()),
            ],
        }),
        "the shared shape predicate must accept the Awe Strike one-shot shape"
    );
    // And the predicate must REJECT the Dromoka leaf (a creature-containing
    // `Typed` is not enough — the type list must be exactly [Creature]).
    assert!(
        !engine::types::ability::is_oneshot_target_source_prevent_shape(&TargetFilter::And {
            filters: vec![
                TargetFilter::ParentTargetSlot { index: 0 },
                TargetFilter::Typed(
                    engine::types::ability::TypedFilter::new(
                        engine::types::ability::TypeFilter::AnyOf(vec![
                            engine::types::ability::TypeFilter::Instant,
                            engine::types::ability::TypeFilter::Sorcery,
                        ])
                    )
                    .with_type(engine::types::ability::TypeFilter::Creature),
                ),
            ],
        }),
        "the shared shape predicate must reject a Typed leaf that is not exactly [Creature]"
    );
}
