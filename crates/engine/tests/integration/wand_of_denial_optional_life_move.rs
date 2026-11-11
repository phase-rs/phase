//! GH #9665 — Wand of Denial: paying the 2 life must put the looked-at card
//! into its owner's graveyard.
//!
//! Oracle text (verbatim, Scryfall): "{T}: Look at the top card of target
//! player's library. If it's a nonland card, you may pay 2 life. If you do, put
//! it into that player's graveyard."
//!
//! CR 701.20e records the looked-at card; CR 608.2c makes "it" that card; CR
//! 119.4 makes the 2 life actual life loss; CR 400.3 puts the card into its
//! owner's graveyard. The declared target is the *player* whose library is
//! looked at, so the chained move must bind the looked-at card from the
//! resolution's reveal ledger, not the inherited player target.

use engine::types::ability::{
    AbilityCondition, AbilityCost, Effect, EffectOutcomeSignal, QuantityExpr, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;

use crate::support::shared_card_db;

/// Wand of Denial (Classic Sixth Edition; verbatim Scryfall Oracle text).
const WAND_OF_DENIAL: &str = "{T}: Look at the top card of target player's library. If it's a nonland card, you may pay 2 life. If you do, put it into that player's graveyard.";

struct WandBoard {
    runner: GameRunner,
    /// The staged top card of P1's library (`None` when the library is empty).
    top: Option<ObjectId>,
}

/// P0 controls Wand of Denial and activates it targeting P1, answering the
/// optional 2-life payment with `accept`. The staged top card of P1's library
/// is `top` with an Island below it, so a move that takes "the top card" is
/// distinguishable from one that takes the whole library.
fn activate_wand(top: Option<&str>, accept: bool) -> Option<WandBoard> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wand = scenario
        .add_artifact_from_oracle(P0, "Wand of Denial", WAND_OF_DENIAL)
        .id();
    let top = top.map(|name| {
        // `add_real_card` appends a library card, so the first staged card is
        // the top (library[0]).
        let id = scenario.add_real_card(P1, name, Zone::Library, db);
        scenario.add_real_card(P1, "Island", Zone::Library, db);
        id
    });
    let mut runner = scenario.build();
    let activation = runner.activate(wand, 0).target_player(P1);
    if accept {
        activation.accept_optional().resolve();
    } else {
        activation.decline_optional().resolve();
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority, got {:?}",
        runner.state().waiting_for
    );
    Some(WandBoard { runner, top })
}

#[test]
fn wand_of_denial_paid_life_moves_the_looked_at_card_to_the_graveyard() {
    let Some(board) = activate_wand(Some("Bonesplitter"), true) else {
        return;
    };
    let top = board.top.expect("staged top card");
    let state = board.runner.state();
    assert_eq!(
        state.last_revealed_ids,
        vec![top],
        "reach guard: the look recorded the top card"
    );
    assert_eq!(state.players[P0.0 as usize].life, 18, "P0 paid 2 life");
    assert_eq!(state.players[P1.0 as usize].life, 20, "P1 paid nothing");
    assert_eq!(
        state.objects[&top].zone,
        Zone::Graveyard,
        "the looked-at card moved to the graveyard"
    );
    assert!(
        state.players[P1.0 as usize].graveyard.contains(&top),
        "the looked-at card is in its owner's graveyard"
    );
    assert!(
        !state.players[P1.0 as usize].library.contains(&top),
        "the looked-at card is no longer in the library"
    );
}

#[test]
fn wand_of_denial_declining_the_payment_leaves_the_card_in_place() {
    let Some(board) = activate_wand(Some("Bonesplitter"), false) else {
        return;
    };
    let top = board.top.expect("staged top card");
    let state = board.runner.state();
    assert_eq!(
        state.last_revealed_ids,
        vec![top],
        "reach guard: the look recorded the top card"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 20,
        "P0 declined; no life"
    );
    assert_eq!(state.objects[&top].zone, Zone::Library);
    assert!(!state.players[P1.0 as usize].graveyard.contains(&top));
}

#[test]
fn wand_of_denial_land_on_top_offers_no_payment() {
    // The accepting row is the control: a land top card never reaches the
    // optional payment, so neither life nor the card moves.
    let Some(board) = activate_wand(Some("Forest"), true) else {
        return;
    };
    let top = board.top.expect("staged top card");
    let state = board.runner.state();
    assert_eq!(
        state.last_revealed_ids,
        vec![top],
        "reach guard: the look recorded the top card"
    );
    assert_eq!(state.players[P0.0 as usize].life, 20, "no payment offered");
    assert_eq!(state.objects[&top].zone, Zone::Library);
    assert!(!state.players[P1.0 as usize].graveyard.contains(&top));
}

#[test]
fn wand_of_denial_empty_library_pays_nothing_and_moves_nothing() {
    let Some(board) = activate_wand(None, true) else {
        return;
    };
    let state = board.runner.state();
    assert_eq!(state.players[P0.0 as usize].life, 20, "no payment offered");
    assert!(
        state.players[P1.0 as usize].graveyard.is_empty(),
        "nothing to move"
    );
    assert!(state.players[P1.0 as usize].library.is_empty());
}

/// The premise this fix rests on: the ability lowers to a look at the targeted
/// player's top card, then an optional 2-life payment gated on the looked-at
/// card not being a land, then a graveyard move anaphoric to the looked-at
/// card ("it"). The move declares no target of its own, so it is the chain's
/// job to bind the looked-at card to it.
#[test]
fn wand_of_denial_parses_as_look_then_optional_life_then_move() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        WAND_OF_DENIAL,
        "Wand of Denial",
        &[],
        &[String::from("Artifact")],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|ability| matches!(&*ability.effect, Effect::Dig { .. }))
        .unwrap_or_else(|| panic!("Wand of Denial must lower to a look: {parsed:#?}"));

    match &*ability.effect {
        Effect::Dig {
            player,
            count,
            keep_count,
            ..
        } => {
            assert_eq!(
                player,
                &TargetFilter::Player,
                "the look is at the TARGET player's library"
            );
            assert_eq!(count, &QuantityExpr::Fixed { value: 1 });
            assert_eq!(
                *keep_count,
                Some(0),
                "the look keeps nothing; the later clause decides the card"
            );
        }
        other => panic!("expected a Dig look head, got {other:#?}"),
    }

    let pay = ability
        .sub_ability
        .as_deref()
        .expect("the look chains to the optional payment");
    assert!(pay.optional, "the 2-life payment is optional: {pay:#?}");
    match &*pay.effect {
        Effect::PayCost { cost, payer, .. } => {
            assert_eq!(
                cost,
                &AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 }
                },
                "the payment is exactly 2 life"
            );
            assert_eq!(
                payer,
                &TargetFilter::Controller,
                "\"you may pay 2 life\": the ability's controller pays, not the targeted player"
            );
        }
        other => panic!("expected a PayCost head, got {other:#?}"),
    }
    match pay.condition.as_ref() {
        Some(AbilityCondition::Not { condition }) => assert!(
            matches!(
                condition.as_ref(),
                AbilityCondition::RevealedHasCardType { card_types, .. }
                    if card_types.as_slice() == [CoreType::Land]
            ),
            "the payment is offered only for a NONLAND top card: {condition:#?}"
        ),
        other => panic!("expected Not(RevealedHasCardType{{Land}}), got {other:#?}"),
    }

    let mv = pay
        .sub_ability
        .as_deref()
        .expect("the payment chains to the move");
    assert_eq!(
        mv.sub_link,
        engine::types::ability::SubAbilityLink::SequentialSibling,
        "the move is a later instruction, not part of the payment"
    );
    match &*mv.effect {
        Effect::ChangeZone {
            destination,
            target,
            ..
        } => {
            assert_eq!(*destination, Zone::Graveyard);
            assert_eq!(
                target,
                &TargetFilter::ParentTarget,
                "\"it\" is the looked-at card, not a freshly declared target"
            );
        }
        other => panic!("expected a ChangeZone{{Graveyard}} rider, got {other:#?}"),
    }
    assert_eq!(
        mv.condition,
        Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed
        }),
        "the move happens only if the 2 life was paid"
    );
}
