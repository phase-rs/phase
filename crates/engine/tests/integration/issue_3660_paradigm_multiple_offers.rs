//! Issue #3660 — Paradigm Spells: only one selection when multiple resolve.
//!
//! When a player has multiple exiled paradigm sources, accepting one
//! `CastParadigmCopy` must re-offer the remaining sources instead of jumping
//! straight to `WaitingFor::Priority`.

use std::sync::Arc;

use engine::game::effects::paradigm::{arm_paradigm, enqueue_offer_if_any};
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn seed_targetless_paradigm_source(
    state: &mut engine::types::game_state::GameState,
    card_num: u64,
    name: &str,
) -> ObjectId {
    let id = create_object(state, CardId(card_num), P0, name.to_string(), Zone::Exile);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(1);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    id
}

/// CR 702.xxx + CR 707.10c: Each paradigm source is offered independently; casting
/// one copy must leave the CastOffer window open for the rest.
#[test]
fn paradigm_cast_re_offers_remaining_sources_through_engine() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut runner = scenario.build();
    let (source_a, source_b) = {
        let state = runner.state_mut();
        let source_a = seed_targetless_paradigm_source(state, 100, "Paradigm Bolt A");
        let source_b = seed_targetless_paradigm_source(state, 101, "Paradigm Bolt B");
        arm_paradigm(state, source_a, P0, "Paradigm Bolt A");
        arm_paradigm(state, source_b, P0, "Paradigm Bolt B");
        assert!(
            enqueue_offer_if_any(state, P0),
            "two paradigm sources must open a cast offer"
        );
        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind: CastOfferKind::Paradigm { offers },
                ..
            } => assert_eq!(offers.len(), 2),
            other => panic!("expected Paradigm CastOffer, got {other:?}"),
        }
        (source_a, source_b)
    };

    runner
        .act(GameAction::CastParadigmCopy { source: source_a })
        .expect("accepting the first paradigm offer must succeed");

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::Paradigm { offers },
        } => {
            assert_eq!(player, P0);
            assert_eq!(offers.len(), 1, "one paradigm source must remain offered");
            assert_eq!(offers[0], source_b);
        }
        other => panic!("expected remaining paradigm CastOffer after first copy, got {other:?}"),
    }

    assert_eq!(
        runner.state().stack.len(),
        1,
        "the accepted paradigm copy must be on the stack"
    );
}

fn seed_targeted_paradigm_source(
    state: &mut engine::types::game_state::GameState,
    card_num: u64,
    name: &str,
) -> ObjectId {
    let id = create_object(state, CardId(card_num), P0, name.to_string(), Zone::Exile);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Sorcery);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(2);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Player,
        },
    ));
    id
}

/// CR 707.10c: Targeted paradigm copies must still re-offer remaining sources
/// after CopyRetarget completes.
#[test]
fn paradigm_targeted_copy_re_offers_after_retarget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut runner = scenario.build();
    let (source_a, source_b) = {
        let state = runner.state_mut();
        let source_a = seed_targeted_paradigm_source(state, 110, "Paradigm Draw A");
        let source_b = seed_targeted_paradigm_source(state, 111, "Paradigm Draw B");
        arm_paradigm(state, source_a, P0, "Paradigm Draw A");
        arm_paradigm(state, source_b, P0, "Paradigm Draw B");
        assert!(enqueue_offer_if_any(state, P0));
        (source_a, source_b)
    };

    runner
        .act(GameAction::CastParadigmCopy { source: source_a })
        .expect("targeted paradigm copy must open CopyRetarget");

    match runner.state().waiting_for.clone() {
        WaitingFor::CopyRetarget { .. } => {}
        other => panic!("expected CopyRetarget for targeted paradigm copy, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(
                engine::types::player::PlayerId(1),
            )),
        })
        .expect("choose opponent target for paradigm copy");

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::Paradigm { offers },
        } => {
            assert_eq!(player, P0);
            assert_eq!(offers.len(), 1);
            assert_eq!(offers[0], source_b);
        }
        other => panic!("expected remaining paradigm offer after retarget, got {other:?}"),
    }
}
