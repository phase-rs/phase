//! Regression for issue #4459: Decoy Gambit's unless-have-you-draw alternative
//! must draw for the spell's controller and suppress the bounce when paid.
//!
//! https://github.com/phase-rs/phase/issues/4459
//!
//! The rows cast `DECOY_GAMBIT_BOUNCE_ORACLE`, a single-target fixture: the
//! verbatim card's "For each opponent, choose up to one target creature …"
//! clause does not reach the unless prompt today. Its unless cost is the same
//! as the verbatim card's, which `decoy_gambit_fixture_carries_the_real_unless_cost`
//! pins.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DECOY_GAMBIT_BOUNCE_ORACLE: &str =
    "Return target creature to its owner's hand unless its controller has you draw a card.";

/// Decoy Gambit, verbatim Oracle text.
const DECOY_GAMBIT_ORACLE: &str = "For each opponent, choose up to one target creature that \
player controls, then return that creature to its owner's hand unless its controller has you \
draw a card.";

/// Narset, Parter of Veils, verbatim Oracle text (Scryfall form, real U+2212 minus).
const NARSET_PARTER_OF_VEILS_ORACLE: &str = "Each opponent can't draw more than one card each \
turn.\n\u{2212}2: Look at the top four cards of your library. You may reveal a noncreature, \
nonland card from among them and put it into your hand. Put the rest on the bottom of your \
library in a random order.";

const STINKWEED_IMP_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a \
creature, destroy that creature.\nDredge 5 (If you would draw a card, you may mill five cards \
instead. If you do, return this card from your graveyard to your hand.)";

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

/// Casts Decoy Gambit at `creature` and resolves it, without asserting what the
/// resolution asks for.
fn cast_decoy_gambit_and_resolve(
    runner: &mut engine::game::scenario::GameRunner,
    gambit: ObjectId,
    creature: ObjectId,
) {
    runner
        .act(GameAction::CastSpell {
            object_id: gambit,
            card_id: runner.state().objects[&gambit].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Decoy Gambit");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Object(creature)],
                    })
                    .expect("target P1's creature");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected pre-resolution prompt: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();
}

fn cast_decoy_gambit_to_unless_prompt(
    runner: &mut engine::game::scenario::GameRunner,
    gambit: ObjectId,
    creature: ObjectId,
) {
    cast_decoy_gambit_and_resolve(runner, gambit, creature);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Decoy Gambit must offer the creature controller an unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
}

/// P0 holds Decoy Gambit with mana to cast it, P1 controls the creature, and
/// `setup` adds whatever the row needs before the board is built.
fn decoy_board(setup: impl FnOnce(&mut GameScenario)) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Top");

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Decoy Gambit", true, DECOY_GAMBIT_BOUNCE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();

    let creature = scenario.add_creature(P1, "Test Bear", 2, 2).id();
    setup(&mut scenario);

    let mut runner = scenario.build();
    add_mana(
        &mut runner,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Blue],
    );
    (runner, gambit, creature)
}

fn setup_at_unless_prompt() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let (mut runner, gambit, creature) = decoy_board(|_| {});
    cast_decoy_gambit_to_unless_prompt(&mut runner, gambit, creature);
    (runner, gambit, creature)
}

#[test]
fn decoy_gambit_declined_unless_payment_bounces_targeted_creature() {
    let (mut runner, _gambit, creature) = setup_at_unless_prompt();
    let hand_before = runner.state().players[P1.0 as usize].hand.len();

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline draw alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Hand,
        "declining the unless cost must return the targeted creature to its owner's hand"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        0,
        "declining the unless cost must not draw for the spell's controller"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        hand_before + 1,
        "declining the unless cost must put the creature in the controller's hand"
    );
}

#[test]
fn decoy_gambit_paid_unless_payment_draws_for_caster_and_spares_creature() {
    let (mut runner, _gambit, creature) = setup_at_unless_prompt();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("accept draw alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "paying the unless cost must prevent the bounce"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        1,
        "paying the unless cost must have the spell's controller draw a card"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|obj| obj.zone == Zone::Hand && obj.name == "Library Top"),
        "the drawn card must come from the caster's library"
    );
}

/// A Decoy Gambit board with Narset, Parter of Veils controlled by
/// `narset_controller`, and `cards_drawn` already drawn this turn by
/// `limited_player`.
fn decoy_board_under_narset(
    narset_controller: PlayerId,
    limited_player: PlayerId,
    cards_drawn: u32,
) -> (GameRunner, ObjectId, ObjectId) {
    let (mut runner, gambit, creature) = decoy_board(|scenario| {
        scenario.add_planeswalker_from_oracle(
            narset_controller,
            "Narset, Parter of Veils",
            "Narset",
            5,
            NARSET_PARTER_OF_VEILS_ORACLE,
        );
    });
    runner.state_mut().players[limited_player.0 as usize].cards_drawn_this_turn = cards_drawn;
    (runner, gambit, creature)
}

/// CR 121.3a + CR 121.2b + CR 614.17b: the payer can't choose the payment when
/// the DRAWER can't draw. P1's Narset limits only P0 (the caster, who would
/// draw), and P0 has already drawn this turn, so P1 is offered no payment and
/// the creature is returned. The payer P1 is unlimited: keying the refusal on
/// the payer would offer the prompt.
#[test]
fn decoy_gambit_payment_is_refused_when_the_drawer_cannot_draw() {
    // Reach guard: with no card drawn yet, the same board offers the payment.
    let (mut runner, gambit, creature) = decoy_board_under_narset(P1, P0, 0);
    cast_decoy_gambit_to_unless_prompt(&mut runner, gambit, creature);

    let (mut runner, gambit, creature) = decoy_board_under_narset(P1, P0, 1);
    let caster = &runner.state().players[P0.0 as usize];
    let (hand_before, library_before) = (caster.hand.len(), caster.library.len());

    cast_decoy_gambit_and_resolve(&mut runner, gambit, creature);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the payment can't be offered while the drawer can't draw, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&creature].zone, Zone::Hand);
    let caster = &runner.state().players[P0.0 as usize];
    assert_eq!(
        caster.library.len(),
        library_before,
        "the caster drew nothing"
    );
    assert_eq!(
        caster.hand.len(),
        hand_before - 1,
        "only Decoy Gambit left the caster's hand"
    );
}

/// CR 121.3a: a draw limit on the PAYER alone does not refuse the payment,
/// because the payer draws nothing. P0's Narset limits only P1, who has already
/// drawn this turn; P1 may still pay, and P0 draws.
#[test]
fn decoy_gambit_payment_is_offered_when_only_the_payer_is_limited() {
    let (mut runner, gambit, creature) = decoy_board_under_narset(P0, P1, 1);
    cast_decoy_gambit_to_unless_prompt(&mut runner, gambit, creature);
    let caster = &runner.state().players[P0.0 as usize];
    let (hand_before, library_before) = (caster.hand.len(), caster.library.len());
    let payer_hand_before = runner.state().players[P1.0 as usize].hand.len();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the payer may have the unlimited caster draw");

    let caster = &runner.state().players[P0.0 as usize];
    assert_eq!(caster.hand.len(), hand_before + 1);
    assert_eq!(caster.library.len(), library_before - 1);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        payer_hand_before
    );
}

/// CR 616.1 + CR 118.11 + CR 118.12: a Dredge choice on the caster's draw
/// pauses the payment. After the choice the payment settles PAID: the creature
/// stays and Decoy Gambit finishes resolving.
#[test]
fn decoy_gambit_payment_paused_by_dredge_settles_paid() {
    let mut imp = None;
    let (mut runner, gambit, creature) = decoy_board(|scenario| {
        scenario.with_library_top(
            P0,
            &[
                "Library 1",
                "Library 2",
                "Library 3",
                "Library 4",
                "Library 5",
            ],
        );
        imp = Some(
            scenario
                .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
                .from_oracle_text(STINKWEED_IMP_ORACLE)
                .id(),
        );
    });
    let imp = imp.expect("Stinkweed Imp was staged");
    cast_decoy_gambit_to_unless_prompt(&mut runner, gambit, creature);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ),
        "Dredge pauses the caster's draw, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_cost_move_resume.is_some(),
        "the paused payment must be parked"
    );

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("applying Dredge is legal");

    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&imp].zone, Zone::Hand);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == gambit
        )),
        "the paid epilogue must finish resolving Decoy Gambit, got {:?}",
        result.events
    );
}

/// SHAPE: the fixture's unless cost is the verbatim card's, so the runtime rows
/// above exercise the cost the real card produces.
#[test]
fn decoy_gambit_fixture_carries_the_real_unless_cost() {
    let types = ["Instant".to_string()];
    let verbatim = parse_oracle_text(DECOY_GAMBIT_ORACLE, "Decoy Gambit", &[], &types, &[]);
    let fixture = parse_oracle_text(DECOY_GAMBIT_BOUNCE_ORACLE, "Decoy Gambit", &[], &types, &[]);
    let verbatim_unless = verbatim.abilities[0].unless_pay.clone();

    assert_eq!(verbatim_unless, fixture.abilities[0].unless_pay.clone());
    let unless = verbatim_unless.expect("Decoy Gambit carries an unless cost");
    assert_eq!(
        unless.cost,
        AbilityCost::EffectCost {
            effect: Box::new(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::OriginalController,
            }),
        }
    );
}
