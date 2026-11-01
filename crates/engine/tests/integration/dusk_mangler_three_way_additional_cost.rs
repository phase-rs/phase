//! CR 601.2b + CR 601.2f + CR 601.2h: Dusk Mangler's three-way additional cost.
//!
//! "As an additional cost to cast this spell, sacrifice a creature, discard a
//! card, or pay 4 life." The caster announces which ONE of the three they pay
//! (CR 601.2b), and only the options they can actually pay are announceable
//! ("Unpayable costs can't be paid", CR 601.2h).
//!
//! Issue #10442: the parser's " or " arm split the line at its last " or " and
//! handed `parse_single_cost` the two-cost fragment "sacrifice a creature,
//! discard a card", which read the sacrifice and silently dropped the discard.
//! `AdditionalCost::Choice` also held exactly two costs, so the third option had
//! nowhere to live. Both tests below drive the real casting pipeline from the
//! printed Oracle text and fail if either half of that fix is reverted:
//!
//! 1. All three options affordable → the prompt offers THREE, and announcing
//!    the middle one (discard) actually discards. On the old two-option parse
//!    the prompt offers two and no answer can reach the discard.
//! 2. No creature and 3 life → discarding is the ONLY payable option, so the
//!    spell must still be castable and must pay it without a prompt. On the old
//!    parse neither surviving option is payable and the cast is refused.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DUSK_MANGLER_ORACLE: &str = "As an additional cost to cast this spell, sacrifice a creature, discard a card, or pay 4 life.\nWhen this creature enters, each opponent sacrifices a creature of their choice, discards a card, and loses 4 life.";
const SWAMP_ORACLE: &str = "({T}: Add {B}.)";

/// Dusk Mangler's printed cost: {5}{B}{B}.
fn dusk_mangler_mana_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 5,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    }
}

/// Seven Swamps — exactly Dusk Mangler's printed {5}{B}{B}, so no slack pays an
/// additional mana cost by accident.
fn add_seven_swamps(scenario: &mut GameScenario) -> Vec<ObjectId> {
    (0..7)
        .map(|_| {
            scenario
                .add_land_from_oracle(P0, "Swamp", SWAMP_ORACLE)
                .with_subtypes(vec!["Swamp"])
                .id()
        })
        .collect()
}

fn add_dusk_mangler(scenario: &mut GameScenario) -> ObjectId {
    let mut builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Dusk Mangler", 5, 4, DUSK_MANGLER_ORACLE);
    builder.with_mana_cost(dusk_mangler_mana_cost());
    builder.with_subtypes(vec!["Zombie", "Demon"]);
    builder.id()
}

/// The three options the printed line lists, in printed order.
fn assert_three_printed_options(cost: &AdditionalCost) {
    let Some(options) = cost.choice_options() else {
        panic!("expected a choice of additional costs, got {cost:?}");
    };
    assert!(
        matches!(
            options,
            [
                AbilityCost::Sacrifice(_),
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    ..
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 4 },
                },
            ]
        ),
        "expected [Sacrifice, Discard 1, PayLife 4] in printed order, got {options:?}"
    );
}

/// CR 601.2b: with every option payable, the caster picks any of the three —
/// including the discard the old parse dropped.
#[test]
fn dusk_mangler_announces_the_discard_option_of_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    let swamps = add_seven_swamps(&mut scenario);

    // A creature to sacrifice (option 0) and a spare card to discard (option 1).
    let fodder = scenario.add_creature(P0, "Sacrifice Fodder", 1, 1).id();
    let spare = scenario.add_card_to_hand(P0, "Discard Fodder");

    let spell = add_dusk_mangler(&mut scenario);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Dusk Mangler must be accepted");

    // CR 601.2h: all three are payable here, so all three are offered.
    let WaitingFor::OptionalCostChoice { cost, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected the additional-cost announcement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_three_printed_options(&cost);

    // CR 601.2b: announce the SECOND option. `DecideOptionalCost { pay }` can
    // only reach the first and the last, which is why the index answer exists.
    runner
        .act(GameAction::ChooseAdditionalCostBranch { index: 1 })
        .expect("announcing the discard option must be accepted");

    let WaitingFor::PayCost {
        kind: PayCostKind::Discard,
        choices,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the discard payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(count, 1, "the printed cost discards exactly one card");
    assert!(
        choices.contains(&spare),
        "the spare card in hand must be an eligible discard, got {choices:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![spare] })
        .expect("discarding the spare card must complete the cost");

    assert_eq!(
        runner.state().objects[&spare].zone,
        Zone::Graveyard,
        "the announced discard must actually be paid"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "the cast must complete once the additional cost is paid"
    );
    // The other two options were NOT paid: the creature survives and no life was lost.
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Battlefield,
        "announcing the discard must not sacrifice the creature too (CR 601.2b: exactly one)"
    );
    assert_eq!(
        runner.state().players[0].life,
        20,
        "announcing the discard must not also pay life"
    );
    for swamp in swamps {
        assert!(
            runner.state().objects[&swamp].tapped,
            "the printed {{5}}{{B}}{{B}} must still be paid ({swamp:?})"
        );
    }
}

/// CR 118.3 + CR 601.2h: a caster with no creature and fewer than 4 life can
/// still cast Dusk Mangler by discarding — the one payable option, which the
/// engine pays without an announcement because there is nothing to choose.
#[test]
fn dusk_mangler_castable_by_discarding_with_no_creature_and_low_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 3);
    add_seven_swamps(&mut scenario);

    let spare = scenario.add_card_to_hand(P0, "Discard Fodder");
    let spell = add_dusk_mangler(&mut scenario);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    assert!(
        can_cast_object_now(runner.state(), PlayerId(0), spell),
        "the discard option must keep Dusk Mangler castable with no creature and 3 life"
    );

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Dusk Mangler must be accepted");

    // CR 601.2b: one payable option leaves nothing to announce, so the engine
    // goes straight to paying it rather than offering a one-button prompt.
    let WaitingFor::PayCost {
        kind: PayCostKind::Discard,
        choices,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the discard payment without an announcement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        choices,
        vec![spare],
        "only the spare card in hand is an eligible discard"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![spare] })
        .expect("discarding the spare card must complete the cost");

    assert_eq!(
        runner.state().objects[&spare].zone,
        Zone::Graveyard,
        "the discard must be paid"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "the cast must complete"
    );
    assert_eq!(
        runner.state().players[0].life,
        3,
        "CR 118.3: the unaffordable 4-life option must not be charged"
    );
}

/// CR 601.2f: a choice of additional costs is part of the total cost, so an
/// additional cost WAS paid whichever option was announced — and also when the
/// announcement was skipped because only one option was payable.
///
/// Building-block fixture rather than Dusk Mangler: a creature spell's stack
/// entry carries no `ResolvedAbility`, so the recorded payment is observable
/// only on a spell whose own ability reaches the stack. The cost text is the
/// same three-way list.
#[test]
fn every_announced_option_records_the_additional_cost_payment() {
    const THREE_WAY_INSTANT: &str = "As an additional cost to cast this spell, sacrifice a creature, discard a card, or pay 4 life.\nDraw a card.";

    /// Builds the fixture, optionally with a creature to sacrifice and a spare
    /// card to discard, and casts it.
    fn cast(
        life: i32,
        creature: bool,
        spare: bool,
    ) -> (
        engine::game::scenario::GameRunner,
        ObjectId,
        Option<ObjectId>,
    ) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, life);
        if creature {
            scenario.add_creature(P0, "Sacrifice Fodder", 1, 1);
        }
        let spare_card = spare.then(|| scenario.add_card_to_hand(P0, "Discard Fodder"));
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Three-Way Cost Specimen",
            true,
            THREE_WAY_INSTANT,
        );
        builder.with_mana_cost(ManaCost::zero());
        let spell = builder.id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the specimen must be accepted");
        (runner, spell, spare_card)
    }

    fn recorded(runner: &engine::game::scenario::GameRunner, spell: ObjectId) -> (bool, u32) {
        let ability = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == spell)
            .and_then(|entry| entry.ability())
            .expect("the specimen is on the stack with its own ability");
        (
            ability.context.additional_cost_paid,
            ability.context.additional_cost_payment_count,
        )
    }

    // Announcing the THIRD option records the payment, exactly as announcing
    // the first does.
    let (mut runner, spell, _) = cast(20, true, true);
    assert!(
        runner.state().waiting_for.pending_cast_ref().is_some(),
        "reach-guard: the announcement is live"
    );
    runner
        .act(GameAction::ChooseAdditionalCostBranch { index: 2 })
        .expect("announcing the pay-4-life option must be accepted");
    assert_eq!(
        runner.state().players[0].life,
        16,
        "the announced option is the one that is paid"
    );
    assert_eq!(
        recorded(&runner, spell),
        (true, 1),
        "CR 601.2f: the third option is a paid additional cost too"
    );

    // The sole payable option is paid with no announcement, and still records.
    let (mut runner, spell, spare) = cast(3, false, true);
    let spare = spare.expect("the spare card exists");
    runner
        .act(GameAction::SelectCards { cards: vec![spare] })
        .expect("the sole payable option is the discard");
    assert_eq!(
        runner.state().objects[&spare].zone,
        Zone::Graveyard,
        "the discard was paid"
    );
    assert_eq!(
        recorded(&runner, spell),
        (true, 1),
        "CR 601.2f: a payment made without an announcement is still a payment"
    );
}

/// CR 601.2b: the indexed answer belongs to a prompt that OFFERS options. On a
/// two-outcome prompt — here Dusk Mangler's own kicker-shaped optional cost — it
/// is refused rather than silently read as "pay", which it would be if the
/// reducer routed it on the prompt kind alone.
#[test]
fn the_indexed_answer_is_refused_on_a_two_outcome_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    add_seven_swamps(&mut scenario);

    // A `you may`-shaped optional additional cost: a two-outcome prompt.
    let mut builder = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Optional Cost Specimen",
        2,
        2,
        "As an additional cost to cast this spell, you may pay 2 life.",
    );
    builder.with_mana_cost(dusk_mangler_mana_cost());
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the optional-cost specimen must be accepted");

    // Reach-guard: the prompt IS the additional-cost announcement, and it offers
    // no option list — so the refusal below is about the answer shape, not about
    // having reached some other prompt.
    let WaitingFor::OptionalCostChoice { cost, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected the additional-cost announcement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        cost.choice_options().is_none(),
        "the specimen's cost must be a two-outcome shape, got {cost:?}"
    );

    runner
        .act(GameAction::ChooseAdditionalCostBranch { index: 0 })
        .expect_err("an indexed answer must not answer a two-outcome prompt");
    assert_eq!(
        runner.state().players[0].life,
        20,
        "the refused answer must not pay the optional life cost"
    );
    // The prompt is still live, so the caster can still answer it properly.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
}

/// CR 601.2h: with every listed option unpayable — no creature, no other card
/// in hand, 3 life — the spell is not castable at all. The complement of the
/// test above: the same three options, the same caster, one resource removed.
#[test]
fn dusk_mangler_uncastable_when_no_listed_option_is_payable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 3);
    add_seven_swamps(&mut scenario);

    // Hand holds Dusk Mangler and nothing else, so the discard has no card.
    let spell = add_dusk_mangler(&mut scenario);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    assert!(
        !can_cast_object_now(runner.state(), PlayerId(0), spell),
        "no payable additional-cost option must make Dusk Mangler uncastable"
    );
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect_err("a directly submitted cast must be refused too");
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "the refused cast must leave the card in hand (CR 733.1)"
    );
}
