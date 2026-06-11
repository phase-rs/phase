//! Regression: "if you can't, draw a card" — Roots of Wisdom
//! ("Mill three cards, then return a land card or Elf card from your graveyard
//! to your hand. If you can't, draw a card.").
//!
//! Bug: the trailing "If you can't, draw a card." sentence parsed as an
//! unconditional `Draw` sub-effect on the chain (`Draw { condition: null }`),
//! so the card ALWAYS drew a card after the return — even on the success
//! branch where a land/Elf was returned. That is a double-payoff.
//!
//! Fix (CR 608.2c): the "if you can't" condition parses as
//! `AbilityCondition::Not { ZoneChangedThisWay { filter: Any } }` and gates the
//! already-present `Draw` in place. `last_zone_changed_ids` is repopulated
//! per-effect, so after the return resolves it holds exactly the returned card
//! (or nothing). The `Draw` therefore fires iff the return moved nothing.
//!
//! Both tests cast the real card and drive the real resolution pipeline:
//!  - return a land  -> `ZoneChangedThisWay` true  -> `Draw` gated false -> 0
//!    cards drawn (the double-draw regression — the critical assertion);
//!  - return nothing -> `ZoneChangedThisWay` false -> `Draw` gated true  -> 1
//!    card drawn.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Cast Roots of Wisdom and drive the stack to completion. The closure
/// `pick_returned` decides, for each `EffectZoneChoice` (the "return a land/Elf
/// card" choice), which cards to return — return `vec![]` to return nothing,
/// or a one-element vec to return that card.
fn resolve_roots_of_wisdom<F>(
    runner: &mut engine::game::scenario::GameRunner,
    roots_id: ObjectId,
    mut pick_returned: F,
) where
    F: FnMut(&[ObjectId]) -> Vec<ObjectId>,
{
    let card_id = runner.state().objects[&roots_id].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: roots_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Roots of Wisdom cast should be accepted");

    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 128, "stack did not settle; last = {result:?}");
        match &result.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                let pick = pick_returned(cards);
                result = runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("resolving the return choice should succeed");
            }
            _ => match runner.act(GameAction::PassPriority) {
                Ok(r) => result = r,
                Err(_) => break,
            },
        }
        if runner.state().stack.is_empty()
            && matches!(result.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
    }
    runner.advance_until_stack_empty();
}

/// Returning a milled land: `ZoneChangedThisWay` is true -> the `Draw` is gated
/// false -> NO card is drawn. This is the double-draw regression assertion.
#[test]
fn roots_of_wisdom_returnable_land_draws_zero() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let roots_id = scenario.add_real_card(P0, "Roots of Wisdom", Zone::Hand, db);
    // A returnable Forest sits in the graveyard so the return moves a card.
    let forest_id = scenario.add_real_card(P0, "Forest", Zone::Graveyard, db);
    // Library padding so Mill 3 and the (gated-off) draw have cards available.
    for _ in 0..6 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, &[ManaType::Black, ManaType::Green]);

    let hand_before = runner.state().players[0].hand.len();
    resolve_roots_of_wisdom(&mut runner, roots_id, |cards| {
        // Return the Forest if it is offered.
        cards
            .iter()
            .copied()
            .filter(|id| *id == forest_id)
            .take(1)
            .collect()
    });
    let hand_after = runner.state().players[0].hand.len();

    // Roots cast (-1), Forest returned to hand (+1), and — critically — NO card
    // drawn. Net hand size unchanged. A double-draw would make this +1.
    assert_eq!(
        hand_after, hand_before,
        "returning the land must NOT also draw a card (double-draw regression); \
         hand {hand_before} -> {hand_after}"
    );
    assert!(
        runner.state().players[0].hand.contains(&forest_id),
        "the returned Forest should be in hand"
    );
}

// ---------------------------------------------------------------------------
// Building-block test of the C1 false branch.
//
// Roots of Wisdom's "return a land/Elf card from your graveyard" Bounce is a
// mandatory effect the engine pre-validates and force-resolves at cast time,
// so the "can't return — therefore draw" path cannot be reached by casting the
// real card (the engine has no castable empty-graveyard route for that Bounce).
// This test instead drives the real `resolve_ability_chain` pipeline — the
// exact code path the C1 condition lives on — with the same `Bounce -> Draw`
// chain Roots of Wisdom produces, gated by the C1 condition
// `Not { ZoneChangedThisWay { Any } }`. It exercises the per-effect
// `last_zone_changed_ids` repopulation and the condition evaluator, proving the
// gated `Draw` fires iff the preceding `Bounce` moved nothing. (`*_draws_zero`
// above covers the success branch end-to-end through a real cast.)
// ---------------------------------------------------------------------------

use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, BounceSelection, ControllerRef, Effect, FilterProp, QuantityExpr,
    ResolvedAbility, TargetFilter, TypedFilter,
};
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;

/// Build the Roots-of-Wisdom-shaped chain: `Bounce` of a land card from the
/// controller's graveyard to hand, with a `Draw 1` sub-ability gated by the C1
/// condition `Not { ZoneChangedThisWay { Any } }`.
fn bounce_then_gated_draw(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let land_in_graveyard = TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::You),
        properties: vec![FilterProp::InZone {
            zone: Zone::Graveyard,
        }],
        ..TypedFilter::land()
    });
    ResolvedAbility::new(
        Effect::Bounce {
            target: land_in_graveyard,
            destination: Some(Zone::Hand),
            selection: BounceSelection::Targeted,
        },
        vec![],
        source,
        controller,
    )
    .sub_ability(
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            controller,
        )
        // CR 608.2c: the C1 condition gating "if you can't, draw a card".
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::ZoneChangedThisWay {
                filter: TargetFilter::Any,
            }),
        }),
    )
}

/// Empty graveyard: the `Bounce` moves nothing -> `Not { ZoneChangedThisWay }`
/// is true -> the gated `Draw` fires exactly once.
#[test]
fn gated_draw_fires_when_bounce_returns_nothing() {
    let mut state = engine::types::game_state::GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(900),
        PlayerId(0),
        "Roots Source".to_string(),
        Zone::Battlefield,
    );
    // Library cards so Draw has something to draw.
    for _ in 0..3 {
        create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Lib".to_string(),
            Zone::Library,
        );
    }
    let hand_before = state.players[0].hand.len();

    let ability = bounce_then_gated_draw(source, PlayerId(0));
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].hand.len(),
        hand_before + 1,
        "empty graveyard: the Bounce returns nothing, so the gated Draw fires once"
    );
}

// The success branch (Bounce returns a card -> `Not { ZoneChangedThisWay }`
// false -> Draw suppressed -> no double-draw) is covered end-to-end through a
// real cast by `roots_of_wisdom_returnable_land_draws_zero` above.
