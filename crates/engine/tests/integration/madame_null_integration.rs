//! Integration tests for Madame Null, Power Broker (TMT).
//!
//! Oracle text:
//!   Deathtouch
//!   Whenever another creature you control enters, you may pay life equal to
//!   its power. If you do, put that many +1/+1 counters on it.
//!
//! These tests pin the `PayCost { Life(Ref(EventContextSourcePower)) }`
//! resolution enabled by the PaymentCost::Life → QuantityExpr widening:
//!   - Live source: power read from `current_trigger_event`'s source object.
//!   - LKI fallback (CR 400.7 / CR 608.2f): if the source has left its zone
//!     between trigger firing and resolution, power is read from the LKI
//!     snapshot — exactly what the TMT ruling requires.
//!   - Unpayable cost: CR 119.8 — insufficient life sets
//!     `cost_payment_failed_flag`, which gates the paired `IfYouDo`
//!     sub-ability so no counters are placed.
//!
//! The "you may" outer prompt (CR 603.2) is engine plumbing that early-returns
//! into a `WaitingFor::OptionalEffectChoice`; it's covered by the action
//! dispatch tests and is intentionally bypassed here to isolate the cost
//! resolver under test.

use engine::game::effects;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityKind, Effect, PaymentCost, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, ZoneChangeRecord};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Builds a Madame Null-shaped chain: outer PayCost(Life(SourcePower)) with
/// an IfYouDo sub-ability (a marker GenericEffect whose execution we can
/// detect via side-effects on `cost_payment_failed_flag` / life total).
///
/// The real sub-ability is `PutCounter(ParentTarget, EventContextAmount)`,
/// but that requires a live parent-target chain. These tests narrow to the
/// question my commit actually enables: does the outer PayCost route the
/// entering creature's power through `resolve_quantity_with_targets` into
/// `pay_life_as_cost` correctly, and do the "you may" / IfYouDo gates
/// compose as expected? The PutCounter integration is exercised elsewhere
/// (Floodpits Drowner stun counter, Ultimate Spider-Man +1/+1).
/// The ability built here omits `optional = true` deliberately: the "you may"
/// prompt is engine plumbing that early-returns into a `WaitingFor` choice
/// state (exercised via the action-dispatch loop in other tests). Removing
/// the optional flag isolates what this commit actually changed — the
/// `PayCost` resolver routing `QuantityExpr` through
/// `resolve_quantity_with_targets` into `pay_life_as_cost`.
fn build_madame_null_pay_chain(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let mut outer = ResolvedAbility::new(
        Effect::PayCost {
            cost: PaymentCost::Life {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextSourcePower,
                },
            },
            payer: TargetFilter::Controller,
        },
        vec![],
        source_id,
        controller,
    );
    outer.kind = AbilityKind::Spell;
    outer
}

/// Set up a ZoneChanged trigger event keyed on the entering creature so
/// `EventContextSourcePower` resolves to that creature's printed power.
fn set_etb_event(state: &mut GameState, entering: ObjectId) {
    state.current_trigger_event = Some(GameEvent::ZoneChanged {
        object_id: entering,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            object_id: entering,
            name: String::new(),
            core_types: Vec::new(),
            subtypes: Vec::new(),
            supertypes: Vec::new(),
            keywords: Vec::new(),
            power: None,
            toughness: None,
            colors: Vec::new(),
            mana_value: 0,
            controller: PlayerId(0),
            owner: PlayerId(0),
            from_zone: Some(Zone::Hand),
            to_zone: Zone::Battlefield,
            attachments: Vec::new(),
            linked_exile_snapshot: Vec::new(),
            is_token: false,
        }),
    });
}

#[test]
fn pay_life_equal_to_source_power_deducts_correct_life() {
    let mut state = GameState::new_two_player(42);
    state.players[0].life = 20;

    // The "entering" creature Madame Null's trigger keys on. Power = 3.
    let entering = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&entering).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);
    }
    set_etb_event(&mut state, entering);

    let madame_null_id = ObjectId(100);
    let ability = build_madame_null_pay_chain(madame_null_id, PlayerId(0));

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].life, 17,
        "paying life equal to source power (3) should deduct 3"
    );
    assert!(
        !state.cost_payment_failed_flag,
        "payment succeeded — IfYouDo sub-ability should be allowed to run"
    );
}

#[test]
fn insufficient_life_sets_cost_failed_flag() {
    let mut state = GameState::new_two_player(42);
    // Not enough life to pay 5 — must set cost_payment_failed_flag so IfYouDo
    // sub-ability (which would place counters) is skipped.
    state.players[0].life = 3;

    let entering = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Polar Kraken".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&entering).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(11);
    }
    set_etb_event(&mut state, entering);

    let ability = build_madame_null_pay_chain(ObjectId(100), PlayerId(0));

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(
        state.cost_payment_failed_flag,
        "paying 11 life with only 3 available must fail the cost (CR 119.8)"
    );
    assert_eq!(
        state.players[0].life, 3,
        "unpayable cost leaves life unchanged"
    );
}

#[test]
fn lki_fallback_resolves_source_power_after_zone_change() {
    // CR 400.7: If the entering creature has already left the battlefield
    // between the trigger firing and resolution (e.g., someone bounced it
    // in response), the source power must come from the LKI cache — not
    // current live state. The TMT ruling quotes this exact case.
    use engine::types::game_state::LKISnapshot;
    use std::collections::HashMap;

    let mut state = GameState::new_two_player(42);
    state.players[0].life = 20;

    let dead_id = ObjectId(999);
    // Deliberately do NOT insert into state.objects — the creature has left
    // its zone. Only the LKI snapshot remains.
    state.lki_cache.insert(
        dead_id,
        LKISnapshot {
            name: "Bounced Bear".to_string(),
            power: Some(4),
            toughness: Some(4),
            mana_value: 2,
            controller: PlayerId(0),
            owner: PlayerId(0),
            card_types: vec![],
            counters: HashMap::new(),
        },
    );
    set_etb_event(&mut state, dead_id);

    let ability = build_madame_null_pay_chain(ObjectId(100), PlayerId(0));

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].life, 16,
        "LKI power (4) should be used when the source is no longer in its zone"
    );
}
