//! Integration tests for issue #562 — Manual Mana Paying: KCI sacrifice-cost
//! mana abilities must be exposed by the castability gate.
//!
//! Krark-Clan Ironworks (KCI) Oracle text:
//!   `Sacrifice an artifact: Add {C}{C}.`
//!
//! KCI is a **non-tap** mana ability (its cost is bare `Sacrifice`, not
//! `Composite { Tap, Sacrifice }`). Before this fix, `can_pay_cost_after_auto_tap`
//! could not see KCI as a mana source — auto-tap only simulates `{T}` activations.
//! That meant `can_cast_object_now` (the castability gate consumed by legal
//! actions) refused to expose `CastSpell { Ichor Wellspring }` even though the
//! player could legally pay `{2}` by manually activating KCI twice.
//!
//! The fix introduces `mana_sources::feasible_mana_capacity` (counts any
//! activatable mana ability, including sacrifice-cost ones) and
//! `casting::can_feasibly_pay_mana_cost` (delegates to auto-tap first, then
//! widens with the manual capacity scan). The legal-actions surface and
//! `max_x_value` route through the new predicate so the castability gate
//! matches what the manual payment flow can actually pay.
//
// CR 117.1d + CR 605.3a + CR 601.2g — mana abilities (including non-tap-cost
// ones) may be activated during cost payment.

use engine::game::apply_as_current;
use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use std::sync::Arc;

const P0: PlayerId = PlayerId(0);

/// Build a KCI-shaped artifact on `player`'s battlefield with the bare
/// `Sacrifice { Typed(Artifact) }: Add {C}{C}` mana ability. Returns its
/// `ObjectId`. Mirrors the canonical KCI ability shape (parser produces the
/// same definition for the real card; see `mana_sources::mana_ability_penalty`
/// docs for KCI's classification).
fn add_kci(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(1001),
        player,
        "Krark-Clan Ironworks".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.summoning_sick = false;
    Arc::make_mut(&mut obj.abilities).push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 2 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            count: 1,
        }),
    );
    id
}

/// Add an artifact creature that can be sacrificed for KCI.
fn add_artifact_creature(
    state: &mut GameState,
    player: PlayerId,
    card_id: u64,
    name: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(1);
    obj.toughness = Some(1);
    obj.summoning_sick = false;
    id
}

/// Add Ichor Wellspring-shape: 2-mana generic artifact in hand. Returns ObjectId.
fn add_two_cost_artifact_to_hand(
    state: &mut GameState,
    player: PlayerId,
    card_id: u64,
    name: &str,
) -> ObjectId {
    let id = create_object(state, CardId(card_id), player, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: 2,
    };
    id
}

/// Put `player` in their PreCombatMain phase with priority. Empty mana pool.
fn priority_main_phase(state: &mut GameState, player: PlayerId) {
    state.phase = Phase::PreCombatMain;
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    state.turn_number = 2;
}

// ---------------------------------------------------------------------------
// (a) Castability gate: `can_cast_object_now` returns true for the
//     Ichor-Wellspring-shape 2-cost artifact when the player has KCI + two
//     sacrificable artifacts.
// ---------------------------------------------------------------------------
//
// CR 117.1d + CR 601.2g + CR 605.3a: KCI is a non-tap mana ability whose cost
// is `Sacrifice an artifact`. With two sacrificable artifacts on the
// battlefield, the player can manually activate KCI twice during cost payment
// to add {C}{C}{C}{C} (more than enough to pay {2}). The castability gate
// (`can_cast_object_now`) must accept the spell — anything less is the #562
// bug.
//
// We assert the castability GATE directly (not the simulated `apply_as_current`
// path) because the simulation oracle runs an Auto-mode cast, which correctly
// fails when only manual mana payment can cover the cost. The frontend
// surfaces the spell via `spell_costs` (built from `display_spell_cost`) and
// dispatches `CastSpellWithPaymentMode { Manual }` — the cast is castable;
// the engine then enters `ManaPayment` and the player activates KCI manually.
#[test]
fn castability_gate_exposes_spell_when_kci_can_feasibly_pay_cost() {
    let mut state = GameState::new_two_player(42);
    priority_main_phase(&mut state, P0);

    let _kci = add_kci(&mut state, P0);
    let _retriever = add_artifact_creature(&mut state, P0, 2001, "Myr Retriever");
    let _trawler = add_artifact_creature(&mut state, P0, 2002, "Scrap Trawler");
    let wellspring = add_two_cost_artifact_to_hand(&mut state, P0, 3001, "Ichor Wellspring");

    // `spell_objects_available_to_cast` is the zone filter — the spell must
    // be one of the player's castable hand objects.
    let available = spell_objects_available_to_cast(&state, P0);
    assert!(
        available.contains(&wellspring),
        "Wellspring must be in spell_objects_available_to_cast (hand zone, owner check)",
    );

    // Issue #562: the load-bearing assertion. Before the fix, this returned
    // `false` because `can_cast_prepared_now` consulted only the auto-tap
    // simulator, which is blind to KCI's non-tap sacrifice cost. With the
    // feasibility predicate, the gate widens to count manual activations.
    assert!(
        can_cast_object_now(&state, P0, wellspring),
        "Issue #562: castability gate must accept Ichor Wellspring when KCI + \
         sacrificable artifacts can feasibly pay {{2}} via manual mana ability \
         activation",
    );
}

/// Add a generic high-cost artifact to a player's hand. Used to drive a cost
/// the feasibility scan cannot cover.
fn add_generic_cost_artifact_to_hand(
    state: &mut GameState,
    player: PlayerId,
    card_id: u64,
    name: &str,
    generic: u32,
) -> ObjectId {
    let id = create_object(state, CardId(card_id), player, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::Cost {
        shards: vec![],
        generic,
    };
    id
}

// ---------------------------------------------------------------------------
// (c) Negative — capacity feasibility respects actual sacrifice supply.
//     `feasible_mana_capacity` is the MAX single-activation yield per
//     permanent, not the total yield assuming infinite re-sacrifice. With
//     only KCI on the battlefield (it can sacrifice itself for 2 mana, but
//     then KCI is gone), the player can pay at most {2}. A 3-cost spell is
//     correctly unreachable.
// ---------------------------------------------------------------------------
//
// CR 601.2b + CR 117.1d + CR 605.3a: One activation of a single mana ability
// contributes at most its single-shot yield. Summing per-permanent capacity
// across the battlefield models "each permanent's mana ability could be
// activated once" — which over-counts in the chain-sacrifice case (KCI
// sacrificing a creature, that creature being a Scrap Trawler triggering
// returns, etc.) but never under-counts the single-shot floor. For pure-KCI
// + no auxiliary sac fodder, the floor is exactly `max_mana_yield = 2`, so
// a {3} cost is unaffordable.
#[test]
fn castability_gate_rejects_spell_when_capacity_below_cost() {
    let mut state = GameState::new_two_player(42);
    priority_main_phase(&mut state, P0);

    // KCI alone — no other artifacts. Best case: KCI sacrifices itself for
    // 2 colorless mana (one activation).
    let _kci = add_kci(&mut state, P0);
    // 3-cost artifact in hand. {3} > KCI's 2-mana single-shot capacity.
    let big_artifact = add_generic_cost_artifact_to_hand(&mut state, P0, 3002, "Big Artifact", 3);

    assert!(
        !can_cast_object_now(&state, P0, big_artifact),
        "Feasibility capacity must respect single-shot yield: with only KCI \
         (max yield 2), a {{3}} cost is unaffordable and the gate must reject \
         the spell",
    );

    // Sanity: a 2-cost spell IS castable from the same setup (KCI's single
    // sacrifice covers exactly {2}), proving the rejection above is about
    // capacity arithmetic — not a general "no spells castable" condition.
    let small_artifact =
        add_generic_cost_artifact_to_hand(&mut state, P0, 3003, "Small Artifact", 2);
    assert!(
        can_cast_object_now(&state, P0, small_artifact),
        "Sanity: a {{2}} cost is still affordable via one KCI activation",
    );
}

// ---------------------------------------------------------------------------
// (d) End-to-end Manual-payment regression. Tests the FULL action chain the
//     castability gate unblocks:
//
//       CastSpellWithPaymentMode { Manual }
//         -> ManaPayment
//         -> ActivateAbility { KCI }
//         -> SacrificeForManaAbility
//         -> SelectCards { the fodder creature }
//         -> ManaPayment (now with {C}{C} in the pool)
//         -> PassPriority   (finalize cost payment)
//         -> Priority       (spell stays on stack, cost paid)
//
// The castability-gate test (a) proves the spell is OFFERED. This test proves
// the downstream Manual-mode flow can actually CONSUME a KCI activation to
// satisfy the cost — i.e. that the gate isn't a vacuous green light. A
// breaking change to the `WaitingFor::ManaPayment` -> `SacrificeForManaAbility`
// -> `ManaPayment` handoff, to `is_active_tap_mana_ability` ignoring non-tap
// mana abilities at cost-payment time, or to `mana_payment::reduce_cost_by_pool`
// colorless resolution would all pass test (a) and fail this one.
// ---------------------------------------------------------------------------
//
// CR 117.1d + CR 601.2g + CR 605.3a + CR 605.3b: A player may activate mana
// abilities (including sacrifice-cost ones) during the cost-payment step;
// each activation resolves immediately, adding its produced mana to the pool
// before the player confirms payment.
#[test]
fn manual_payment_flow_resolves_kci_sacrifice_to_pay_spell_cost() {
    let mut state = GameState::new_two_player(42);
    priority_main_phase(&mut state, P0);

    let kci = add_kci(&mut state, P0);
    let retriever = add_artifact_creature(&mut state, P0, 2001, "Myr Retriever");
    let _trawler = add_artifact_creature(&mut state, P0, 2002, "Scrap Trawler");
    let wellspring = add_two_cost_artifact_to_hand(&mut state, P0, 3001, "Ichor Wellspring");
    let wellspring_card_id = state.objects[&wellspring].card_id;

    // Step 1: Cast Ichor Wellspring with Manual payment mode. The engine
    // moves the object to the stack (CR 601.2a) and pauses on ManaPayment
    // for the caster to activate mana abilities (CR 601.2g).
    apply_as_current(
        &mut state,
        GameAction::CastSpellWithPaymentMode {
            object_id: wellspring,
            card_id: wellspring_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    )
    .expect("CastSpellWithPaymentMode { Manual } must succeed when KCI can feasibly pay {2}");

    match &state.waiting_for {
        WaitingFor::ManaPayment { player, .. } => assert_eq!(*player, P0),
        other => panic!("Expected WaitingFor::ManaPayment after Manual cast, got {other:?}"),
    }
    // CR 601.2a: the spell has moved from hand to the stack at announcement time.
    assert!(
        state.stack.iter().any(|entry| entry.id == wellspring),
        "Wellspring should be on the stack after announcement"
    );

    // Step 2: Activate KCI's `Sacrifice an artifact: Add {C}{C}` ability.
    // The sacrifice target is the only choice surfaced — KCI has no other
    // cost components — so the engine transitions to SacrificeForManaAbility.
    apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: kci,
            ability_index: 0,
        },
    )
    .expect("ActivateAbility(KCI) during ManaPayment must succeed");

    match &state.waiting_for {
        WaitingFor::SacrificeForManaAbility {
            player,
            count,
            permanents,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*count, 1);
            // CR 117.1: legal sacrifice candidates are artifacts (per the
            // ability's TargetFilter). All three artifacts the controller owns
            // are eligible — including KCI itself, which is the worst case
            // for chain-sacrifice analysis but legal here.
            assert!(
                permanents.contains(&retriever),
                "Retriever must appear as a legal sacrifice candidate"
            );
        }
        other => panic!("Expected SacrificeForManaAbility after activating KCI, got {other:?}"),
    }

    // Step 3: Sacrifice the Retriever. KCI resolves and adds {C}{C} to the
    // pool (CR 605.3b), the state transitions back to ManaPayment for the
    // caster to confirm.
    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![retriever],
        },
    )
    .expect("SelectCards for sacrifice target must succeed");

    match &state.waiting_for {
        WaitingFor::ManaPayment { player, .. } => assert_eq!(*player, P0),
        other => panic!("Expected ManaPayment after KCI resolution, got {other:?}"),
    }
    // CR 605.3b: KCI's resolution put {C}{C} (two colorless mana) into the pool.
    let pool_total = state.players[P0.0 as usize].mana_pool.total();
    assert_eq!(
        pool_total, 2,
        "KCI activation must add {{C}}{{C}} to the mana pool — got {pool_total} total mana"
    );
    // CR 117.1 + CR 701.16a: Retriever should now be in the graveyard
    // (sacrificed as the activation cost).
    assert!(
        state.players[P0.0 as usize].graveyard.contains(&retriever),
        "Retriever must be in graveyard after being sacrificed for KCI"
    );

    // Step 4: Pass priority to finalize the mana payment. The engine debits
    // {2} from the pool, leaves the spell on the stack, and returns priority
    // to the caster (CR 117.3a).
    apply_as_current(&mut state, GameAction::PassPriority)
        .expect("PassPriority during ManaPayment must finalize payment");

    // After cost payment: pool empty, spell still on stack waiting for both
    // players to pass priority for resolution. We don't drive resolution
    // here — the load-bearing assertion for #562 is that the cost was paid
    // via Manual mode + KCI, not that Wellspring entered the battlefield.
    let pool_after_pay = state.players[P0.0 as usize].mana_pool.total();
    assert_eq!(
        pool_after_pay, 0,
        "Mana pool should be empty after the {{2}} cost is debited — got {pool_after_pay}",
    );
    assert!(
        state.stack.iter().any(|entry| entry.id == wellspring),
        "Wellspring should still be on the stack after cost is paid (awaiting resolution)"
    );
}
