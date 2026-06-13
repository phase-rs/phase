//! Regression for issue #328: Gatta and Luzzu's damage-prevention ETB was
//! reported as failing — the targeted creature died because (a) the shield
//! was being installed on Gatta itself instead of the chosen target, (b) the
//! prevention shield was depletion-based (`Next(1)`) so it absorbed only the
//! first damage event, and (c) the rider's "it" anaphor in
//! "put that many +1/+1 counters on it" was binding to `SelfRef` (Gatta)
//! rather than the parent's chosen target.
//!
//! Oracle text:
//!     "Flash
//!      When Gatta and Luzzu enters, choose target creature you control. If
//!      damage would be dealt to that creature this turn, prevent that damage
//!      and put that many +1/+1 counters on it."
//!
//! This test pins the full chained-trigger shape end-to-end:
//!   TargetOnly { Creature, You }
//!     → sub_ability: PreventDamage { All, ParentTarget, AllDamage }
//!         duration: UntilEndOfTurn
//!         → sub_ability: PutCounter { P1P1, target: ParentTarget }
//!             repeat_for: EventContextAmount
//!
//! And the runtime contracts:
//!   - The `Prevention { All }` shield persists across multiple damage events
//!     (CR 615.1a — only `Next(N)` shields are depletion-based per CR 615.7).
//!   - The shield is hosted on the *chosen* creature, not Gatta (CR 608.2c —
//!     `ParentTarget` aliases to the parent's selected target).
//!   - Each prevented damage event accumulates `+1/+1` counters on the chosen
//!     creature, one per 1 damage prevented (CR 615.5 — additional effect that
//!     refers to the prevented amount, fired immediately after each prevention).
//!
//! CR 608.2c: Later instructions may refer to a target chosen earlier in the
//!            same effect.
//! CR 615.1a: Effects that use the word "prevent" are prevention effects.
//! CR 615.5:  Prevention effects may include an additional effect that refers
//!            to the amount of damage prevented; the additional effect runs
//!            immediately after the prevention.
//! CR 615.7:  `Prevent the next N damage` is a depletion shield. (Distinct
//!            from this card's `Prevent that damage` formulation.)
//! CR 514.2:  "This turn" effects end at the cleanup step.

use engine::game::effects;
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, PreventionAmount, PreventionScope, QuantityExpr, QuantityRef, ResolvedAbility,
    ShieldKind, TargetFilter, TargetRef,
};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Build the chained `PreventDamage → PutCounter` sub-ability that Gatta and
/// Luzzu's parser produces, parameterized on `chosen` so each test can wire
/// the parent target into both `ability.targets` propagation slots.
fn build_gatta_prevention_chain(
    gatta: engine::types::identifiers::ObjectId,
    chosen: engine::types::identifiers::ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    let mut counter_rider = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
        },
        vec![TargetRef::Object(chosen)],
        gatta,
        controller,
    );
    counter_rider.repeat_for = Some(QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    });

    let mut prevent = ResolvedAbility::new(
        Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::ParentTarget,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
            prevention_duration: None,
        },
        vec![TargetRef::Object(chosen)],
        gatta,
        controller,
    )
    .sub_ability(counter_rider);
    prevent.duration = Some(engine::types::ability::Duration::UntilEndOfTurn);
    prevent
}

/// CR 608.2c + CR 615.1a + CR 615.5: End-to-end Gatta and Luzzu — choose a
/// creature, install a persistent prevention shield on it, fire three damage
/// events, and confirm zero damage marked + three sets of `+1/+1` counters
/// equal to the total damage that *would have been* dealt (12).
#[test]
fn gatta_and_luzzu_prevents_three_damage_events_and_accumulates_counters() {
    let mut state = GameState::new_two_player(42);

    let gatta = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Gatta and Luzzu".to_string(),
        Zone::Battlefield,
    );
    let chosen = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    let attacker = create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "Goblin".to_string(),
        Zone::Battlefield,
    );

    // Resolve the prevention sub-ability — installs the shield on the chosen
    // creature with EOT expiry and stashes the counter rider as the
    // post-replacement continuation.
    let prevent = build_gatta_prevention_chain(gatta, chosen, PlayerId(0));
    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &prevent, &mut events, 0).unwrap();

    // Shield must land on the chosen creature, not on Gatta.
    let chosen_obj = state.objects.get(&chosen).unwrap();
    assert_eq!(
        chosen_obj.replacement_definitions.len(),
        1,
        "shield must be hosted on the chosen target — got {:?}",
        chosen_obj.replacement_definitions
    );
    assert!(matches!(
        chosen_obj.replacement_definitions[0].shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        }
    ));
    // CR 514.2 cleanup contract: every `ShieldKind != None` is pruned at the
    // cleanup step via `ShieldKind::is_shield()` in `turns::execute_cleanup`,
    // independently of the explicit `expiry` field. The duration plumbing on
    // the ability is therefore advisory; the shield-kind sentinel is what
    // guarantees EOT cleanup for prevention shields.
    assert!(
        chosen_obj.replacement_definitions[0]
            .shield_kind
            .is_shield(),
        "prevention shield must register as a shield for EOT cleanup"
    );

    let gatta_obj = state.objects.get(&gatta).unwrap();
    assert!(
        gatta_obj.replacement_definitions.is_empty(),
        "shield must NOT be installed on Gatta — got {:?}",
        gatta_obj.replacement_definitions
    );

    // Fire three damage events of varying sizes (4, 1, 7 — total 12) by
    // resolving an `Effect::DealDamage` ability whose source is the attacker.
    // Each prevention event must (a) absorb all damage, (b) re-fire the
    // rider adding counters equal to the prevented amount.
    let damage_amounts: [i32; 3] = [4, 1, 7];
    let mut expected_counters: u32 = 0;
    for dmg in damage_amounts {
        let damage_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: dmg },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(chosen)],
            attacker,
            PlayerId(1),
        );
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &damage_ability, &mut events, 0).unwrap();

        expected_counters += dmg as u32;
        let chosen_obj = state.objects.get(&chosen).unwrap();
        assert_eq!(
            chosen_obj.damage_marked, 0,
            "no damage should be marked after prevention (dmg={dmg})"
        );
        let counters = chosen_obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counters, expected_counters,
            "P1P1 counters must accumulate to {expected_counters} after dmg {dmg}; got {counters}"
        );
    }

    // Total: 12 damage prevented → 12 P1P1 counters on the chosen creature.
    let chosen_obj = state.objects.get(&chosen).unwrap();
    assert_eq!(
        chosen_obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        12,
        "total accumulated P1P1 counters must equal total prevented damage"
    );
    assert_eq!(chosen_obj.damage_marked, 0);

    // Shield must STILL be present after all three events — `Prevention { All }`
    // is duration-bound, not depletion-bound (CR 615.1a).
    let chosen_obj = state.objects.get(&chosen).unwrap();
    assert_eq!(
        chosen_obj.replacement_definitions.len(),
        1,
        "shield must persist across all three damage events"
    );
    assert!(
        !chosen_obj.replacement_definitions[0].is_consumed,
        "Prevention {{ All }} must not be consumed by use — got consumed shield"
    );
}

/// CR 615.1a + CR 615.5: Pinwheel test — confirm that 4 damage in one event
/// vs. 4 separate 1-damage events both produce 4 counters total. This locks
/// in the per-event accumulation model: counters scale with the prevented
/// amount per event, not per damage point.
#[test]
fn gatta_and_luzzu_pinwheel_one_event_vs_split_events_yield_same_total() {
    fn run_with_damage_profile(events_to_fire: &[i32]) -> u32 {
        let mut state = GameState::new_two_player(42);
        let gatta = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gatta and Luzzu".to_string(),
            Zone::Battlefield,
        );
        let chosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        let prevent = build_gatta_prevention_chain(gatta, chosen, PlayerId(0));
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &prevent, &mut events, 0).unwrap();

        for dmg in events_to_fire {
            let damage_ability = ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: *dmg },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
                vec![TargetRef::Object(chosen)],
                attacker,
                PlayerId(1),
            );
            let mut events = Vec::new();
            effects::resolve_ability_chain(&mut state, &damage_ability, &mut events, 0).unwrap();
        }
        state
            .objects
            .get(&chosen)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    let single_event = run_with_damage_profile(&[4]);
    let split_events = run_with_damage_profile(&[1, 1, 1, 1]);
    assert_eq!(
        single_event, 4,
        "one 4-damage event prevented must add 4 P1P1 counters"
    );
    assert_eq!(
        split_events, 4,
        "four 1-damage events prevented must add 4 P1P1 counters total"
    );
    assert_eq!(
        single_event, split_events,
        "per-event accumulation must yield the same total as one big event"
    );
}
