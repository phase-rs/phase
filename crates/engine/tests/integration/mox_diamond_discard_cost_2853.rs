//! Mox Diamond — the as-enters "you may discard a land card instead" cost must
//! actually discard a land when accepted, and only then does the artifact enter.
//!
//! Regression for issue #2853: the `MayCost { Discard }` replacement parsed
//! correctly (accept/decline branches present), but the runtime accept path
//! routed the discard through `pay_ability_cost` in *activation* scope. The
//! `Discard { FromHand, Chosen }` shape is only paid in *resolution* scope; in
//! activation scope it fell through to the interactive-pass-through arm and
//! returned `Paid` as a silent no-op. The land was never discarded, yet the
//! artifact still entered (the cost was skipped). These tests drive the real
//! parsed replacement through the as-enters pipeline and assert the land is
//! discarded on accept, kept on decline, and that an unpayable accept routes
//! the artifact to the graveyard.
//!
//! CR 614.12a / CR 614.12: a declined/unpayable alternative routes to the
//! owner's graveyard; CR 701.9a: discarding moves a card from hand to graveyard.
//!
//! CR 614.17b + CR 118.12: a MayCost that includes an impossible draw is
//! refused at the accept; a resumed paid remainder is not re-gated.
//!
//! CR 614.12a + CR 614.11a + CR 616.1: an accepted MayCost whose leg pauses on
//! that leg's own replacement choice (a Dredge or skip on a draw leg, an
//! ordering choice on the discarded land) keeps that record live, and the
//! permanent enters only after the leg, every later leg and the whole draw
//! instruction have finished — and before the enclosing effect continues.

use engine::game::effects::change_zone::resolve;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, Effect, QuantityExpr, ReplacementMode, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, PendingCostMoveResume, PendingReplacement, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::resolution::{FrameKind, ResolutionFrame};
use engine::types::zones::Zone;

use crate::draw_from_general_post_replacement::{
    choose_replacement, frame_kinds, APPLY_REPLACEMENT, EIGHT_CARD_LIBRARY, STINKWEED_IMP_ORACLE,
};

const MOX_DIAMOND_ORACLE: &str =
    "If this artifact would enter, you may discard a land card instead. If you do, \
     put this artifact onto the battlefield. If you don't, put it into its owner's \
     graveyard.";

/// Move a card from hand to the battlefield, surfacing its as-enters `MayCost`
/// replacement choice (mirrors the production ETB pathway for Mox Diamond).
fn enter_via_change_zone(runner_state: &mut engine::types::game_state::GameState, card: ObjectId) {
    let mut events = Vec::new();
    resolve(runner_state, &entry_from_hand(card), &mut events)
        .expect("Mox Diamond enter resolves into a MayCost pause");
}

/// The effect instruction "put `card` from your hand onto the battlefield".
fn entry_from_hand(card: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        engine::types::ability::Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Battlefield,
            target: TargetFilter::SpecificObject { id: card },
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        ObjectId(9000),
        PlayerId(0),
    )
}

/// CR 614.12a + CR 701.9a: accepting the discard cost must discard a land from
/// hand and only then put Mox Diamond onto the battlefield.
#[test]
fn mox_diamond_accept_discards_land_and_enters() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    // The as-enters replacement pauses for the accept/decline choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    // Accept (index 0): pay the discard cost. With exactly one land in hand the
    // discard is forced — no further choice round-trip.
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the Mox Diamond discard cost should resolve");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the replacement choice must be consumed after accepting"
    );
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Graveyard,
        "accepting the cost must discard the land to its owner's graveyard, \
         not leave it in hand"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "after paying the discard cost, Mox Diamond enters the battlefield"
    );
}

/// CR 614.12a: declining sends Mox Diamond to its owner's graveyard and never
/// discards a land.
#[test]
fn mox_diamond_decline_routes_to_graveyard_without_discard() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let decline = candidates
        .iter()
        .position(|c| c.description.contains("Decline"))
        .expect("decline option must be offered");

    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("declining should resolve");

    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Graveyard,
        "a declined Mox Diamond is routed to its owner's graveyard"
    );
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Hand,
        "declining must not discard a land"
    );
}

/// CR 614.12a: accepting with no land to discard is an unpayable cost — it falls
/// through to the decline branch (Mox to graveyard), never entering for free.
#[test]
fn mox_diamond_accept_unpayable_routes_to_graveyard() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting an unpayable cost should resolve");

    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Graveyard,
        "an unpayable Mox Diamond discard cost falls through to the graveyard \
         redirect — it must never enter the battlefield for free"
    );
}

/// CR 701.9a: with more than one eligible land, the discard requires a genuine
/// choice. The accept path must surface that choice (not silently auto-pick or
/// no-op); only after a land is actually discarded does Mox Diamond enter.
#[test]
fn mox_diamond_accept_with_multiple_lands_requires_choice() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest_a = scenario.add_land_to_hand(P0, "Forest").id();
    let forest_b = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the discard cost should resolve");

    // Two lands: the cost is not forced, so the engine must ask which land to
    // discard before the artifact can enter.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "expected a DiscardChoice for the two-land case, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land to discard should resolve");

    assert_eq!(
        runner.state().objects[&forest_a].zone,
        Zone::Graveyard,
        "the chosen land is discarded"
    );
    assert_eq!(
        runner.state().objects[&forest_b].zone,
        Zone::Hand,
        "the un-chosen land stays in hand"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "Mox Diamond enters after the discard choice is committed"
    );
}

/// CR 614.12a + CR 118.12: if a composite MayCost pauses for an interactive
/// discard choice, the post-choice resume must still pay the remaining suffix
/// before the replacement applies.
#[test]
fn may_cost_discard_choice_resume_pays_remaining_composite_suffix() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest_a = scenario.add_land_to_hand(P0, "Forest").id();
    let forest_b = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    {
        let obj = runner.state_mut().objects.get_mut(&mox).unwrap();
        let replacement_index = obj
            .replacement_definitions
            .iter_unchecked()
            .position(|definition| matches!(definition.mode, ReplacementMode::MayCost { .. }))
            .expect("Mox Diamond replacement should parse as MayCost");
        let replacement = &mut obj.replacement_definitions[replacement_index];
        let (discard_cost, decline) = match &replacement.mode {
            ReplacementMode::MayCost { cost, decline } => (cost.clone(), decline.clone()),
            other => panic!("expected MayCost, got {other:?}"),
        };
        replacement.mode = ReplacementMode::MayCost {
            cost: AbilityCost::Composite {
                costs: vec![
                    discard_cost,
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                ],
            },
            decline,
        };
    }

    enter_via_change_zone(runner.state_mut(), mox);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the composite MayCost should surface discard choice");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "expected a DiscardChoice for the two-land composite case, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land to discard should resume the remaining cost");

    assert_eq!(
        runner.state().objects[&forest_a].zone,
        Zone::Graveyard,
        "the chosen land is discarded"
    );
    assert_eq!(
        runner.state().objects[&forest_b].zone,
        Zone::Hand,
        "the un-chosen land stays in hand"
    );
    assert_eq!(
        runner.state().players[0].life,
        18,
        "the remaining PayLife suffix must be paid after the discard choice"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "Mox Diamond enters only after all composite cost components are paid"
    );
}

/// Maralen of the Mornsong, verbatim Oracle text: a CR 121.3 can't-draw effect.
const MARALEN_ORACLE: &str = "Players can't draw cards.\nAt the beginning of each player's draw \
step, that player loses 3 life, searches their library for a card, puts it into their hand, then shuffles.";

/// P0's library: draw outcomes are read on its size (6 = no card drawn).
const STAGED_LIBRARY: [&str; 6] = ["L1", "L2", "L3", "L4", "L5", "L6"];

/// One one-card draw instruction by the controller.
fn draw_cost(cards: i32) -> AbilityCost {
    AbilityCost::EffectCost {
        effect: Box::new(Effect::Draw {
            count: QuantityExpr::Fixed { value: cards },
            target: TargetFilter::Controller,
        }),
    }
}

/// When Maralen joins the board in a MayCost draw row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MaralenArrival {
    Never,
    BeforeTheAccept,
    AtTheDiscardChoice,
}

/// A Mox Diamond whose MayCost is `Composite[<parsed discard>, Draw 1]` (the composite precedent above,
/// with its PayLife leg swapped for a draw leg), two Forests in P0's hand, and P0's library staged. With
/// `AtTheDiscardChoice`, Maralen is built and moved to exile so the caller can return her mid-payment.
/// Returns the runner, Mox Diamond, both Forests and Maralen (if built).
fn mox_with_a_draw_leg(
    arrival: MaralenArrival,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &STAGED_LIBRARY);
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest_a = scenario.add_land_to_hand(P0, "Forest").id();
    let forest_b = scenario.add_land_to_hand(P0, "Forest").id();
    let maralen = (arrival != MaralenArrival::Never).then(|| {
        scenario
            .add_creature_from_oracle(P1, "Maralen of the Mornsong", 2, 3, MARALEN_ORACLE)
            .id()
    });
    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    if let (MaralenArrival::AtTheDiscardChoice, Some(maralen)) = (arrival, maralen) {
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            maralen,
            Zone::Exile,
            &mut Vec::new(),
        );
    }

    let obj = runner.state_mut().objects.get_mut(&mox).unwrap();
    let replacement_index = obj
        .replacement_definitions
        .iter_unchecked()
        .position(|definition| matches!(definition.mode, ReplacementMode::MayCost { .. }))
        .expect("Mox Diamond replacement should parse as MayCost");
    let replacement = &mut obj.replacement_definitions[replacement_index];
    let (discard_cost, decline) = match &replacement.mode {
        ReplacementMode::MayCost { cost, decline } => (cost.clone(), decline.clone()),
        other => panic!("expected MayCost, got {other:?}"),
    };
    replacement.mode = ReplacementMode::MayCost {
        cost: AbilityCost::Composite {
            costs: vec![discard_cost, draw_cost(1)],
        },
        decline,
    };
    (runner, mox, forest_a, forest_b, maralen)
}

fn library_size(runner: &GameRunner) -> usize {
    runner.state().players[0].library.len()
}

/// CR 614.12a + CR 118.12: control row for the draw-leg MayCost — with no can't-draw effect, the accept
/// pays the discard leg (after its choice) and then the draw leg, and Mox Diamond enters.
#[test]
fn a_composite_may_cost_with_a_draw_leg_pays_every_leg() {
    let (mut runner, mox, forest_a, _forest_b, _) = mox_with_a_draw_leg(MaralenArrival::Never);
    enter_via_change_zone(runner.state_mut(), mox);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the composite MayCost is legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land resumes the remaining draw leg");
    assert_eq!(runner.state().objects[&mox].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&forest_a].zone, Zone::Graveyard);
    assert_eq!(
        library_size(&runner),
        STAGED_LIBRARY.len() - 1,
        "the draw leg drew a card"
    );
}

/// CR 614.17b + CR 121.3: accepting is the choice to pay, and a cost that includes a draw Maralen forbids
/// can't be chosen. The accept is refused as a whole before any leg is paid, so the "If you don't" branch
/// puts Mox Diamond into its owner's graveyard.
///
/// Revert probe: passing `LatchedSuffix` from the accept branch skips the fresh gate, and the accept raises
/// a `DiscardChoice`.
#[test]
fn a_may_cost_including_an_impossible_draw_is_refused_at_accept() {
    let (mut runner, mox, forest_a, forest_b, _) =
        mox_with_a_draw_leg(MaralenArrival::BeforeTheAccept);
    enter_via_change_zone(runner.state_mut(), mox);
    // Reach guard: the MayCost replacement was offered.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting resolves");

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "no leg of a refused choice is started"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Graveyard,
        "the refused cost routes Mox Diamond to its owner's graveyard"
    );
    assert_eq!(runner.state().objects[&forest_a].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&forest_b].zone, Zone::Hand);
    assert_eq!(library_size(&runner), STAGED_LIBRARY.len());
    assert!(runner.state().pending_replacement.is_none());
}

/// CR 118.12 + CR 614.17a + CR 118.11: Maralen arrives after the accept, while the discard leg's choice is
/// open. The accepted MayCost latched the choice, so its remaining draw leg is paid without re-gating —
/// clamped as it happens — and Mox Diamond enters.
///
/// Revert probe: passing `FreshChoice` from the resume branch re-gates the draw leg, which is refused, and
/// Mox Diamond goes to its owner's graveyard.
#[test]
fn a_may_cost_draw_leg_resumed_after_a_discard_choice_stays_paid() {
    let (mut runner, mox, forest_a, forest_b, maralen) =
        mox_with_a_draw_leg(MaralenArrival::AtTheDiscardChoice);
    enter_via_change_zone(runner.state_mut(), mox);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting is legal while no can't-draw effect exists");

    // Reach guards: the resume will enter the paid-cost branch carrying the draw leg.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the accepted MayCost is parked at the discard choice");
    assert!(pending.may_cost_paid);
    assert_eq!(pending.may_cost_remaining, Some(draw_cost(1)));

    let maralen = maralen.expect("Maralen was built for this row");
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        maralen,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land resumes the remaining draw leg");

    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "the latched remainder is paid"
    );
    assert_eq!(runner.state().objects[&forest_a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&forest_b].zone, Zone::Hand);
    assert_eq!(
        library_size(&runner),
        STAGED_LIBRARY.len(),
        "the draw leg was paid and clamped by Maralen"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert!(!runner.state().replacement_may_cost_paused);
}

// An accepted MayCost whose leg pauses on that leg's own replacement choice.
//
// CR 614.12a: the entry choice, and so its cost, is settled before the permanent enters. CR 614.11a +
// CR 121.6b: a replaced draw is completed before the draw instruction resumes. The leg's own replacement
// record must stay live for its answer while the accepted entry replacement waits, with its unpaid later
// legs, in `PendingCostMoveResume::ReplacementMayCostInnerChoice`.

/// Obstinate Familiar, verbatim Oracle text: an optional skip of a single draw.
const OBSTINATE_FAMILIAR_ORACLE: &str = "If you would draw a card, you may skip that draw instead.";

/// Rest in Peace, verbatim Oracle text.
const REST_IN_PEACE_ORACLE: &str =
    "When this enchantment enters, exile all graveyards.\nIf a card or token \
would be put into a graveyard from anywhere, exile it instead.";

/// Leyline of the Void, verbatim Oracle text.
const LEYLINE_OF_THE_VOID_ORACLE: &str = "If this card is in your opening hand, you may begin the game with \
it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

/// At an optional replacement prompt, index 1 declines the replacement.
const DECLINE_REPLACEMENT: usize = 1;

/// Which leg of the rewritten Mox Diamond MayCost is paid first.
#[derive(Clone, Copy)]
enum LegOrder {
    DiscardThenDraw,
    DrawThenDiscard,
}

/// The replacement effects that can apply to the draw leg.
#[derive(Clone, Copy)]
enum DrawReplacer {
    StinkweedImp,
    ObstinateFamiliar,
    Both,
    Unreplaced,
}

/// A Mox Diamond whose MayCost is rewritten to a draw leg and its parsed discard leg, in a chosen order.
struct ReplacedDrawLegBoard {
    runner: GameRunner,
    mox: ObjectId,
    lands: Vec<ObjectId>,
    imp: Option<ObjectId>,
    /// The parsed discard leg: the exact unpaid remainder a paused draw leg leaves behind it.
    discard_leg: AbilityCost,
}

/// Rewrite the card's parsed MayCost on the card in hand (the composite precedent above), returning the
/// parsed discard leg.
fn rewrite_may_cost_legs(
    state: &mut GameState,
    mox: ObjectId,
    legs: impl FnOnce(AbilityCost) -> Vec<AbilityCost>,
) -> AbilityCost {
    let object = state.objects.get_mut(&mox).expect("Mox Diamond exists");
    let replacement_index = object
        .replacement_definitions
        .iter_unchecked()
        .position(|definition| matches!(definition.mode, ReplacementMode::MayCost { .. }))
        .expect("Mox Diamond replacement should parse as MayCost");
    let replacement = &mut object.replacement_definitions[replacement_index];
    let (discard_leg, decline) = match &replacement.mode {
        ReplacementMode::MayCost { cost, decline } => (cost.clone(), decline.clone()),
        other => panic!("expected MayCost, got {other:?}"),
    };
    replacement.mode = ReplacementMode::MayCost {
        cost: AbilityCost::Composite {
            costs: legs(discard_leg.clone()),
        },
        decline,
    };
    discard_leg
}

/// P0 in their main phase with Mox Diamond, `land_count` Forests in hand and the eight-card library. The
/// MayCost becomes `Composite[draw <cards>, <parsed discard>]` in `order`, and `replacer` places Stinkweed Imp
/// in P0's graveyard and/or Obstinate Familiar on P0's battlefield.
fn mox_with_a_replaced_draw_leg(
    order: LegOrder,
    land_count: usize,
    cards: i32,
    replacer: DrawReplacer,
) -> ReplacedDrawLegBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &EIGHT_CARD_LIBRARY);
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let mut lands = Vec::new();
    for _ in 0..land_count {
        lands.push(scenario.add_land_to_hand(P0, "Forest").id());
    }
    let with_imp = matches!(replacer, DrawReplacer::StinkweedImp | DrawReplacer::Both);
    let imp = with_imp.then(|| {
        scenario
            .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text(STINKWEED_IMP_ORACLE)
            .id()
    });
    if matches!(
        replacer,
        DrawReplacer::ObstinateFamiliar | DrawReplacer::Both
    ) {
        scenario.add_creature_from_oracle(
            P0,
            "Obstinate Familiar",
            1,
            1,
            OBSTINATE_FAMILIAR_ORACLE,
        );
    }
    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    let discard_leg = rewrite_may_cost_legs(runner.state_mut(), mox, |discard| match order {
        LegOrder::DiscardThenDraw => vec![discard, draw_cost(cards)],
        LegOrder::DrawThenDiscard => vec![draw_cost(cards), discard],
    });
    ReplacedDrawLegBoard {
        runner,
        mox,
        lands,
        imp,
        discard_leg,
    }
}

/// How many times `object` moved to `zone` across the accumulated events.
fn moves_of(events: &[GameEvent], object: ObjectId, zone: Zone) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::ZoneChanged { object_id, to, .. } if *object_id == object && *to == zone)
        })
        .count()
}

/// The order of `object`'s battlefield entry and every life change across the accumulated events.
fn entry_and_life_order(events: &[GameEvent], object: ObjectId) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                ..
            } if *object_id == object => Some("entry"),
            GameEvent::LifeChanged { .. } => Some("life"),
            _ => None,
        })
        .collect()
}

/// The permanent spell's resolution frame survives the inner prompt, buried beneath the draw leg's frame.
fn assert_spell_resolution_buried(state: &GameState, mox: ObjectId) {
    let frames: Vec<&ResolutionFrame> = state.resolution_stack.iter().collect();
    assert!(
        matches!(
            frames.as_slice(),
            [ResolutionFrame::SpellResolution(spell), ResolutionFrame::MultiDraw(_)]
                if spell.object_id == mox
        ),
        "expected [SpellResolution(mox), MultiDraw], got {:?}",
        frame_kinds(state)
    );
}

/// The accepted entry replacement parked in its typed continuation while the inner record is answered.
fn parked_outer_replacement(state: &GameState) -> &PendingReplacement {
    let Some(PendingCostMoveResume::ReplacementMayCostInnerChoice { outer_replacement }) =
        state.pending_cost_move_resume.as_ref()
    else {
        panic!(
            "the accepted entry replacement must wait in its typed continuation, got {:?}",
            state.pending_cost_move_resume
        );
    };
    assert!(outer_replacement.may_cost_paid, "the outer was accepted");
    assert!(
        matches!(outer_replacement.proposed, ProposedEvent::ZoneChange { .. }),
        "the parked outer is the entry"
    );
    outer_replacement
}

/// Nothing is left parked once the entry has completed.
fn assert_settled(state: &GameState) {
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_cost_move_resume.is_none(), "no owner remains");
    assert!(state.pending_replacement.is_none(), "no record remains");
    assert!(!state.replacement_may_cost_paused);
    assert!(
        state.active_draw_sequence().is_none(),
        "the draw instruction finished"
    );
    assert!(
        state.resolution_stack.is_empty(),
        "no resolution frame leaks, got {:?}",
        frame_kinds(state)
    );
}

/// CR 614.12a + CR 614.11a + CR 702.52a: the draw leg's Dredge record stays live while the accepted entry
/// replacement waits with its unpaid discard leg; either answer completes the draw, then the discard, and
/// only then does Mox Diamond enter, exactly once. The apply row round-trips the state through serde at the
/// prompt and answers on the restored runner.
///
/// Revert probes: without the owner install, the prompt is the outer `ZoneChange` and the decline answer
/// sends Mox Diamond to its owner's graveyard. Without the draw driver's frame retirement, the apply row
/// leaves `[SpellResolution(mox)]` on the resolution stack.
#[test]
fn an_accepted_may_cost_waits_for_dredge_on_its_draw_leg() {
    for answer in [APPLY_REPLACEMENT, DECLINE_REPLACEMENT] {
        let ReplacedDrawLegBoard {
            mut runner,
            mox,
            lands,
            imp,
            discard_leg,
        } = mox_with_a_replaced_draw_leg(
            LegOrder::DrawThenDiscard,
            1,
            1,
            DrawReplacer::StinkweedImp,
        );
        let forest = lands[0];
        let imp = imp.expect("Stinkweed Imp was built for this row");

        let mut events = runner.cast(mox).resolve().events().to_vec();
        // Reach guard: the outer entry replacement was offered.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        choose_replacement(&mut runner, 0, &mut events);

        let state = runner.state();
        let WaitingFor::ReplacementChoice { player, .. } = state.waiting_for else {
            panic!("expected the Dredge prompt, got {:?}", state.waiting_for);
        };
        assert_eq!(player, P0);
        let inner = state
            .pending_replacement
            .as_ref()
            .expect("the draw leg's replacement record owns the slot");
        assert!(
            matches!(inner.proposed, ProposedEvent::Draw { .. }),
            "the live prompt is the draw leg's own Dredge, got {:?}",
            inner.proposed
        );
        assert_eq!(
            parked_outer_replacement(state).may_cost_remaining,
            Some(discard_leg.clone()),
            "the unpaid discard leg waits with the outer"
        );
        assert_eq!(state.objects[&mox].zone, Zone::Stack, "no premature entry");
        assert_eq!(
            state.objects[&forest].zone,
            Zone::Hand,
            "the discard leg is unpaid"
        );
        assert_spell_resolution_buried(state, mox);

        if answer == APPLY_REPLACEMENT {
            let serialized =
                serde_json::to_string(runner.state()).expect("the prompt state serializes");
            let restored: GameState =
                serde_json::from_str(&serialized).expect("the prompt state restores");
            assert!(matches!(
                restored.pending_cost_move_resume,
                Some(PendingCostMoveResume::ReplacementMayCostInnerChoice { .. })
            ));
            runner = GameRunner::from_state(restored);
        }
        choose_replacement(&mut runner, answer, &mut events);

        let state = runner.state();
        if answer == APPLY_REPLACEMENT {
            assert_eq!(
                state.objects[&imp].zone,
                Zone::Hand,
                "Dredge returned the Imp"
            );
            assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 5);
        } else {
            assert_eq!(state.objects[&imp].zone, Zone::Graveyard);
            assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 1);
        }
        assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
        assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
        assert_eq!(
            moves_of(&events, mox, Zone::Battlefield),
            1,
            "entry happens exactly once"
        );
        assert_eq!(
            moves_of(&events, forest, Zone::Graveyard),
            1,
            "the land is discarded once"
        );
        assert_settled(state);
    }
}

/// CR 614.12a + CR 118.12: control row — an unreplaced draw leg pays straight through on the cast path.
#[test]
fn an_unreplaced_draw_leg_pays_every_leg_on_the_cast_path() {
    let ReplacedDrawLegBoard {
        mut runner,
        mox,
        lands,
        ..
    } = mox_with_a_replaced_draw_leg(LegOrder::DrawThenDiscard, 1, 1, DrawReplacer::Unreplaced);
    let mut events = runner.cast(mox).resolve().events().to_vec();
    choose_replacement(&mut runner, 0, &mut events);

    let state = runner.state();
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(state.objects[&lands[0]].zone, Zone::Graveyard);
    assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 1);
    assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1);
    assert_settled(state);
}

/// CR 614.12a + CR 118.12 + CR 702.52a: the review's literal scaffold — the discard leg's choice is answered,
/// the latched draw leg then pauses on Dredge, and the outer waits with nothing left unpaid. Neither answer
/// may complete the entry before the draw settles.
///
/// Revert probe: without the owner install, the decline answer declines the outer entry and Mox Diamond goes
/// to its owner's graveyard; the apply answer leaves the library untouched.
#[test]
fn a_may_cost_draw_leg_resumed_after_a_discard_choice_waits_for_dredge() {
    for answer in [APPLY_REPLACEMENT, DECLINE_REPLACEMENT] {
        let ReplacedDrawLegBoard {
            mut runner,
            mox,
            lands,
            imp,
            ..
        } = mox_with_a_replaced_draw_leg(
            LegOrder::DiscardThenDraw,
            2,
            1,
            DrawReplacer::StinkweedImp,
        );
        let (forest_a, forest_b) = (lands[0], lands[1]);
        let imp = imp.expect("Stinkweed Imp was built for this row");
        enter_via_change_zone(runner.state_mut(), mox);
        let mut events = Vec::new();
        choose_replacement(&mut runner, 0, &mut events);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { .. }
        ));
        let result = runner
            .act(GameAction::SelectCards {
                cards: vec![forest_a],
            })
            .expect("selecting the land resumes the latched draw leg");
        events.extend(result.events);

        let state = runner.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(matches!(
            state
                .pending_replacement
                .as_ref()
                .map(|inner| &inner.proposed),
            Some(ProposedEvent::Draw { .. })
        ));
        assert_eq!(
            parked_outer_replacement(state).may_cost_remaining,
            None,
            "the draw leg is the last leg, so nothing waits behind it"
        );
        assert_eq!(state.objects[&mox].zone, Zone::Hand, "no premature entry");

        choose_replacement(&mut runner, answer, &mut events);

        let state = runner.state();
        if answer == APPLY_REPLACEMENT {
            assert_eq!(state.objects[&imp].zone, Zone::Hand);
            assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 5);
        } else {
            assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 1);
        }
        assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
        assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1);
        assert_eq!(state.objects[&forest_a].zone, Zone::Graveyard);
        assert_eq!(state.objects[&forest_b].zone, Zone::Hand);
        assert_settled(state);
    }
}

/// CR 614.12a + CR 118.12: once the draw leg settles, a later interactive discard leg re-parks the outer in
/// the free replacement slot and its choice finishes the entry once.
#[test]
fn a_dredge_declined_draw_leg_hands_its_later_discard_choice_back_to_the_outer() {
    let ReplacedDrawLegBoard {
        mut runner,
        mox,
        lands,
        discard_leg,
        ..
    } = mox_with_a_replaced_draw_leg(LegOrder::DrawThenDiscard, 2, 1, DrawReplacer::StinkweedImp);
    let (forest_a, forest_b) = (lands[0], lands[1]);
    enter_via_change_zone(runner.state_mut(), mox);
    let mut events = Vec::new();
    choose_replacement(&mut runner, 0, &mut events);
    assert_eq!(
        parked_outer_replacement(runner.state()).may_cost_remaining,
        Some(discard_leg),
        "the unpaid discard leg waits with the outer"
    );

    choose_replacement(&mut runner, DECLINE_REPLACEMENT, &mut events);
    let state = runner.state();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    assert!(
        state
            .pending_replacement
            .as_ref()
            .is_some_and(|outer| outer.may_cost_paid),
        "the outer is back in the free replacement slot for the discard hook"
    );
    assert!(state.pending_cost_move_resume.is_none());
    assert_eq!(state.objects[&mox].zone, Zone::Hand);

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land finishes the cost");
    events.extend(result.events);
    let state = runner.state();
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1);
    assert_eq!(state.objects[&forest_a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&forest_b].zone, Zone::Hand);
    assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 1);
    assert_settled(state);
}

/// CR 121.2 + CR 614.11a: a skipped first draw of a two-card draw leg finishes the instruction (its second
/// draw) before the outer resumes, on the cast path.
///
/// Revert probe: without the Prevented-arm draw gate, the outer resumes after the first skip — Mox Diamond
/// is on the battlefield and the Forest discarded while the second draw is still owed.
#[test]
fn a_skipped_draw_leg_unit_does_not_let_the_permanent_enter_early() {
    let ReplacedDrawLegBoard {
        mut runner,
        mox,
        lands,
        discard_leg,
        ..
    } = mox_with_a_replaced_draw_leg(
        LegOrder::DrawThenDiscard,
        1,
        2,
        DrawReplacer::ObstinateFamiliar,
    );
    let forest = lands[0];
    let mut events = runner.cast(mox).resolve().events().to_vec();
    choose_replacement(&mut runner, 0, &mut events);
    assert_eq!(
        parked_outer_replacement(runner.state()).may_cost_remaining,
        Some(discard_leg.clone())
    );

    // Skip the first draw.
    choose_replacement(&mut runner, APPLY_REPLACEMENT, &mut events);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the second draw's skip prompt is open, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&mox].zone, Zone::Stack, "no premature entry");
    assert_eq!(state.objects[&forest].zone, Zone::Hand);
    assert_eq!(
        parked_outer_replacement(state).may_cost_remaining,
        Some(discard_leg)
    );
    assert_spell_resolution_buried(state, mox);

    // Draw the second card.
    choose_replacement(&mut runner, DECLINE_REPLACEMENT, &mut events);
    let state = runner.state();
    assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 1);
    assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1);
    assert_settled(state);
}

/// CR 616.1 + CR 614.12a + CR 701.9a: printed Mox Diamond cast with one Forest while its opponent controls
/// Rest in Peace and Leyline of the Void. The forced discard asks for the order of the two graveyard
/// redirects; that live record is answered first, the land is exiled, and only then does Mox Diamond enter.
///
/// Revert probe: without the owner install, the outer overwrites the ordering record — the Forest stays in
/// hand while Mox Diamond enters.
#[test]
fn mox_diamond_waits_for_the_discarded_lands_replacement_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE_ORACLE);
    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID_ORACLE);
    let mut runner = scenario.build();

    let mut events = runner.cast(mox).resolve().events().to_vec();
    choose_replacement(&mut runner, 0, &mut events);

    let state = runner.state();
    let inner = state
        .pending_replacement
        .as_ref()
        .expect("the discarded land's ordering record owns the slot");
    assert_eq!(inner.proposed.affected_object_id(), Some(forest));
    assert!(!inner.is_optional);
    assert_eq!(inner.candidates.len(), 2);
    assert_eq!(
        parked_outer_replacement(state).may_cost_remaining,
        None,
        "the printed discard is the only leg"
    );
    assert_eq!(frame_kinds(state), vec![FrameKind::SpellResolution]);
    assert_eq!(state.objects[&mox].zone, Zone::Stack, "no premature entry");

    choose_replacement(&mut runner, 0, &mut events);

    let state = runner.state();
    assert_eq!(state.objects[&forest].zone, Zone::Exile);
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1);
    assert_settled(state);
}

/// The draw leg's replacement in an entry-ordering row: which replacer, and the answer to its prompt.
#[derive(Clone, Copy, Debug)]
enum DrawLegAnswer {
    DredgeApplied,
    DredgeDeclined,
    Skipped,
}

/// CR 608.2c + CR 614.12a: the permanent enters before the effect's next instruction. An effect puts Mox
/// Diamond onto the battlefield and then gains 3 life; the accepted MayCost's draw leg pauses on its own
/// replacement. Whatever the answer, the entry precedes the life gain. The Dredge rows settle on the
/// delivered arm, the skip row on the prevented arm.
///
/// Revert probe: without the rider exclusion, the Dredge rows order the life gain before the entry (the
/// DredgeApplied row fails first, with `["life", "entry"]`). The skip row is a measurement, not a
/// discriminator of that exclusion: the prevented arm has no rider drain ahead of its cost drain.
#[test]
fn an_entry_may_cost_finishes_before_the_effects_next_instruction() {
    for draw_leg in [
        DrawLegAnswer::DredgeApplied,
        DrawLegAnswer::DredgeDeclined,
        DrawLegAnswer::Skipped,
    ] {
        let (replacer, answer, library_after) = match draw_leg {
            DrawLegAnswer::DredgeApplied => (DrawReplacer::StinkweedImp, APPLY_REPLACEMENT, 3),
            DrawLegAnswer::DredgeDeclined => (DrawReplacer::StinkweedImp, DECLINE_REPLACEMENT, 7),
            DrawLegAnswer::Skipped => (DrawReplacer::ObstinateFamiliar, APPLY_REPLACEMENT, 8),
        };
        let ReplacedDrawLegBoard {
            mut runner,
            mox,
            lands,
            discard_leg,
            ..
        } = mox_with_a_replaced_draw_leg(LegOrder::DrawThenDiscard, 1, 1, replacer);
        let entry_then_life = entry_from_hand(mox).sub_ability(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        ));
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &entry_then_life, &mut events, 0)
            .expect("the effect pauses on Mox Diamond's entry replacement");
        choose_replacement(&mut runner, 0, &mut events);

        let state = runner.state();
        assert_eq!(
            state.players[0].life, 20,
            "{draw_leg:?}: the life gain waits"
        );
        assert_eq!(
            state.objects[&mox].zone,
            Zone::Hand,
            "{draw_leg:?}: no premature entry"
        );
        assert_eq!(
            parked_outer_replacement(state).may_cost_remaining,
            Some(discard_leg),
            "{draw_leg:?}"
        );

        choose_replacement(&mut runner, answer, &mut events);

        let state = runner.state();
        assert_eq!(
            entry_and_life_order(&events, mox),
            vec!["entry", "life"],
            "{draw_leg:?}: the entry precedes the effect's next instruction"
        );
        assert_eq!(state.players[0].life, 23, "{draw_leg:?}");
        assert_eq!(library_size(&runner), library_after, "{draw_leg:?}");
        assert_eq!(
            state.objects[&lands[0]].zone,
            Zone::Graveyard,
            "{draw_leg:?}"
        );
        assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1, "{draw_leg:?}");
        assert_settled(state);
    }
}

/// CR 614.11a + CR 702.52a + CR 616.1: a two-card draw leg whose first draw is dredged (chosen over the
/// Familiar's skip) and whose second draw is skipped. The skip settles on the prevented arm with the
/// Dredge's emptied substitute frame exposed beneath the finished draw, and the entry still completes the
/// permanent spell's resolution.
///
/// Revert probe: without the draw driver's frame retirement, `[SpellResolution(mox)]` is left on the stack.
#[test]
fn a_dredged_then_skipped_draw_leg_completes_the_spell_resolution() {
    let ReplacedDrawLegBoard {
        mut runner,
        mox,
        lands,
        imp,
        discard_leg,
    } = mox_with_a_replaced_draw_leg(LegOrder::DrawThenDiscard, 1, 2, DrawReplacer::Both);
    let forest = lands[0];
    let imp = imp.expect("Stinkweed Imp was built for this row");
    let mut events = runner.cast(mox).resolve().events().to_vec();
    choose_replacement(&mut runner, 0, &mut events);

    // CR 616.1: the first draw has two applicable replacements; choose the Imp's Dredge.
    let candidates = &runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the first draw's ordering record owns the slot")
        .candidates;
    assert_eq!(candidates.len(), 2);
    let dredge = candidates
        .iter()
        .position(|candidate| candidate.source == imp)
        .expect("the Imp's Dredge is offered");
    choose_replacement(&mut runner, dredge, &mut events);
    choose_replacement(&mut runner, APPLY_REPLACEMENT, &mut events);

    let state = runner.state();
    assert_eq!(state.objects[&imp].zone, Zone::Hand);
    assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 5);
    assert_eq!(state.objects[&mox].zone, Zone::Stack, "no premature entry");
    assert_eq!(
        parked_outer_replacement(state).may_cost_remaining,
        Some(discard_leg)
    );
    assert_eq!(
        frame_kinds(state),
        vec![
            FrameKind::SpellResolution,
            FrameKind::PostReplacement,
            FrameKind::MultiDraw
        ],
        "the Dredge's substitute frame sits beneath the unfinished draw"
    );

    // Skip the second draw.
    choose_replacement(&mut runner, APPLY_REPLACEMENT, &mut events);

    let state = runner.state();
    assert_eq!(library_size(&runner), EIGHT_CARD_LIBRARY.len() - 5);
    assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(moves_of(&events, mox, Zone::Battlefield), 1);
    assert_settled(state);
}
