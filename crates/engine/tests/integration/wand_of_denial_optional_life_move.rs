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
//! look's result carried through the payment, not the inherited player target.

use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, Effect, EffectOutcomeSignal,
    QuantityExpr, SubAbilityLink, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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
    /// The player the optional 2-life payment was offered to, if it was.
    offered: Option<PlayerId>,
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
    runner
        .activate(wand, 0)
        .target_player(P1)
        .stop_at_optional()
        .resolve();
    // CR 118.12 + CR 608.2d: "you may pay 2 life" is the Wand controller's
    // choice, not the targeted library owner's. Only a nonland top card reaches
    // the payment at all.
    let offered =
        if let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for {
            assert_eq!(player, P0, "the Wand's controller answers the payment");
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .expect("answer the optional 2-life payment");
            Some(player)
        } else {
            None
        };
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority, got {:?}",
        runner.state().waiting_for
    );
    Some(WandBoard {
        runner,
        top,
        offered,
    })
}

#[test]
fn wand_of_denial_paid_life_moves_the_looked_at_card_to_the_graveyard() {
    let Some(board) = activate_wand(Some("Bonesplitter"), true) else {
        return;
    };
    assert_eq!(
        board.offered,
        Some(P0),
        "a nonland top card offers the payment to the controller"
    );
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
    assert_eq!(
        board.offered,
        Some(P0),
        "a nonland top card offers the payment to the controller"
    );
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
    assert_eq!(board.offered, None, "a land top card offers no payment");
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
    assert_eq!(board.offered, None, "an empty library offers no payment");
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
        SubAbilityLink::SequentialSibling,
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

/// Wand of Denial's printed payment, parsed from its Oracle text: the optional
/// 2-life `PayCost` with its "If you do, put it into that player's graveyard"
/// move. A `SequentialSibling` guard (the controller loses 1 life) follows the
/// move, so a run that drops the move still proves the chain kept resolving.
fn wand_payment_then_move_then_guard() -> AbilityDefinition {
    let parsed = engine::parser::oracle::parse_oracle_text(
        WAND_OF_DENIAL,
        "Wand of Denial",
        &[],
        &[String::from("Artifact")],
        &[],
    );
    let mut pay = *parsed.abilities[0]
        .sub_ability
        .clone()
        .expect("the look chains to the optional payment");
    let mut guard = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: Some(TargetFilter::Controller),
        },
    );
    guard.sub_link = SubAbilityLink::SequentialSibling;
    pay.sub_ability
        .as_mut()
        .expect("the payment chains to the move")
        .sub_ability = Some(Box::new(guard));
    pay
}

/// Activates `probe`'s `{T}` ability (optionally targeting `target`) and
/// accepts the optional 2-life payment, which must be offered to P0.
fn activate_probe_and_pay(runner: &mut GameRunner, probe: ObjectId, target: Option<ObjectId>) {
    let activation = runner.activate(probe, 0);
    let activation = match target {
        Some(target) => activation.target_object(target),
        None => activation,
    };
    activation.stop_at_optional().resolve();
    match runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(player, P0, "the controller answers the payment")
        }
        ref other => panic!("expected the optional 2-life payment, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the optional 2-life payment");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the activation resolved");
}

/// CR 608.2c + CR 609.3: "it" is what the instruction immediately before the
/// payment produced, and an instruction that produced nothing leaves "it" with
/// no referent, even while an earlier reveal's card is still in the reveal
/// ledger. Chain: reveal P0's top card A -> turn the (already face-up) source
/// face up, which produces nothing -> Wand's paid move. Only the payment and
/// the guard happen; A stays on top of the library.
#[test]
fn payment_after_an_empty_producer_moves_nothing_despite_an_earlier_reveal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let earlier = scenario.add_card_to_library_top(P0, "Earlier Reveal");
    let mut turn_up = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TurnFaceUp {
            target: TargetFilter::SelfRef,
        },
    );
    turn_up.sub_ability = Some(Box::new(wand_payment_then_move_then_guard()));
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::RevealTop {
            player: TargetFilter::Controller,
            count: 1,
        },
    )
    .cost(AbilityCost::Tap);
    ability.sub_ability = Some(Box::new(turn_up));
    let probe = {
        let mut builder = scenario.add_artifact_from_oracle(P0, "Probe", "");
        builder.with_ability_definition(ability);
        builder.id()
    };
    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&probe].face_down,
        "precondition: the source is already face up, so turning it up produces nothing"
    );

    activate_probe_and_pay(&mut runner, probe, None);

    let state = runner.state();
    assert_eq!(
        state.last_revealed_ids,
        vec![earlier],
        "reach guard: the earlier reveal's card is still in the ledger"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 17,
        "P0 paid 2 life and the trailing guard took 1"
    );
    assert_eq!(
        state.objects[&earlier].zone,
        Zone::Library,
        "the earlier revealed card must not move"
    );
    assert_eq!(state.players[P0.0 as usize].library.front(), Some(&earlier));
    assert!(state.players[P0.0 as usize].graveyard.is_empty());
    assert_eq!(state.objects[&probe].zone, Zone::Battlefield);
}

/// CR 608.2c + CR 400.7: the payment carries the producer's exact result, not
/// an object target the producer merely inherited. Chain: target creature A ->
/// reveal the source B ("reveal ~", which names the source, not A) -> Wand's
/// paid move. Only B moves to the graveyard; A stays on the battlefield.
#[test]
fn payment_moves_only_the_producers_object_not_an_inherited_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let inherited = scenario.add_creature(P1, "Inherited Target", 2, 2).id();
    let mut reveal = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Reveal {
            target: TargetFilter::SelfRef,
        },
    );
    reveal.sub_ability = Some(Box::new(wand_payment_then_move_then_guard()));
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
        },
    )
    .cost(AbilityCost::Tap);
    ability.sub_ability = Some(Box::new(reveal));
    let probe = {
        let mut builder = scenario.add_artifact_from_oracle(P0, "Probe", "");
        builder.with_ability_definition(ability);
        builder.id()
    };
    let mut runner = scenario.build();

    activate_probe_and_pay(&mut runner, probe, Some(inherited));

    let state = runner.state();
    assert_eq!(
        state.last_revealed_ids,
        vec![probe],
        "reach guard: the reveal produced the source"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 17,
        "P0 paid 2 life and the trailing guard took 1"
    );
    assert_eq!(
        state.objects[&probe].zone,
        Zone::Graveyard,
        "the revealed source moved"
    );
    assert!(state.players[P0.0 as usize].graveyard.contains(&probe));
    assert_eq!(
        state.objects[&inherited].zone,
        Zone::Battlefield,
        "the inherited target is not the payment's result and must not move"
    );
    assert!(state.players[P1.0 as usize].graveyard.is_empty());
}
