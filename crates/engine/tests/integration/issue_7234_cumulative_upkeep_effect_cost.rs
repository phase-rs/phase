//! GitHub issue #7234 — Cumulative upkeep must pay typed source-counter
//! effect costs after card-data/save-state deserialization.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, Effect, EffectKind, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::sync::Arc;

const ABOROTH_ORACLE: &str =
    "Cumulative upkeep—Put a -1/-1 counter on this creature. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)";

const SOLEMNITY_ORACLE: &str = "Players can't get counters.\n\
Counters can't be put on artifacts, creatures, enchantments, or lands.";

const VORINCLEX_ORACLE: &str = "Trample, haste\n\
If you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.\n\
If an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

const DOC_SAMSON_ORACLE: &str = "If you would put one or more counters on a permanent you control, put that many plus one of each of those kinds of counters on that permanent instead.\n\
{T}: Add X mana of any one color, where X is Doc Samson's power.";

/// CR 702.24a: Card-data and saved games use the externally tagged keyword
/// form. A typed Aboroth effect cost must not be replaced by a zero-mana cost.
#[test]
fn cumulative_upkeep_typed_effect_cost_survives_deserialization() {
    let keyword: Keyword = serde_json::from_str(
        r#"{"CumulativeUpkeep":{"type":"EffectCost","effect":{"type":"PutCounter","counter_type":"M1M1","count":{"type":"Fixed","value":1},"target":{"type":"SelfRef"}}}}"#,
    )
    .expect("typed CumulativeUpkeep payload deserializes");

    assert!(matches!(
        keyword,
        Keyword::CumulativeUpkeep(AbilityCost::EffectCost { effect })
            if matches!(
                effect.as_ref(),
                Effect::PutCounter {
                    counter_type: CounterType::Minus1Minus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }
            )
    ));
}

/// CR 702.24a: Aboroth's effect-as-cost is paid once per age counter. With one
/// pre-existing age counter, the upkeep tick makes two and paying the prompt
/// must place two -1/-1 counters while keeping Aboroth on the battlefield.
#[test]
fn aboroth_cumulative_upkeep_scales_and_pays_source_counter_effect_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let aboroth = scenario
        .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aboroth)
        .expect("Aboroth exists")
        .counters
        .insert(CounterType::Age, 1);

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { cost, .. } => assert!(matches!(
            cost,
            AbilityCost::EffectCost {
                effect,
            } if matches!(
                effect.as_ref(),
                Effect::PutCounter {
                    counter_type: CounterType::Minus1Minus1,
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::SelfRef,
                }
            )
        )),
        other => panic!("expected Aboroth's cumulative-upkeep payment prompt, got {other:?}"),
    }

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("Aboroth's counter cost is payable");

    let aboroth_object = runner
        .state()
        .objects
        .get(&aboroth)
        .expect("Aboroth remains");
    assert_eq!(aboroth_object.zone, Zone::Battlefield);
    assert_eq!(aboroth_object.counters.get(&CounterType::Age), Some(&2));
    assert_eq!(
        aboroth_object.counters.get(&CounterType::Minus1Minus1),
        Some(&2),
        "paying the cumulative cost must place one -1/-1 counter for each age counter"
    );
}

/// CR 702.24a + CR 616.1 + CR 118.12: Aboroth's cumulative upkeep is an
/// `AbilityCost::EffectCost` — the *second* unless-payment park site, the
/// sibling of Ward's `AbilityCost::GetPlayerCounters` site. When two printed
/// replacement effects both modify the counter placement the payer is paying
/// WITH, CR 616.1 makes the payer order them, and the payment PARKS mid-cost on
/// `PendingCostMoveResume::CounterAdditionUnlessPayment`.
///
/// CR 616.1 genuinely applies here because the two modifications do not commute
/// on the placement's count: Vorinclex is multiplicative (`Times{2}`) and Doc
/// Samson is additive (`Plus{1}`), so a 3-counter cost settles at 3 → 6 → 7 with
/// Vorinclex first and 3 → 4 → 8 with Doc Samson first.
///
/// CR 118.12: whichever order is chosen, the cost is PAID — the "if they don't"
/// clause "checks whether the player chose to pay an optional cost … regardless
/// of what events actually occurred", and the choice was latched at
/// `PayUnlessCost { pay: true }`, before the replacement pipeline was consulted.
/// CR 118.11 corroborates: a cost whose payment actions were modified is still
/// paid. So the guarded "sacrifice it" never happens.
///
/// `GameEvent::EffectResolved { kind: EffectKind::Sacrifice, .. }` here means
/// "the cumulative-upkeep ability finished resolving", NOT that Aboroth was
/// sacrificed. The paired `zone == Zone::Battlefield` assertion is what proves
/// the permanent survived.
#[test]
fn aboroth_cumulative_upkeep_payment_ordered_by_two_replacements_is_still_paid() {
    // Both CR 616.1 orderings are driven inline in one test function rather than
    // through a parameterised helper: the ordering IS the axis under test, and a
    // single function keeps the ordering assertions beside the counter totals
    // they explain.
    for (index, expected_minus_counters) in [(0usize, 7u32), (1usize, 8u32)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        scenario.add_creature_from_oracle(
            P0,
            "Vorinclex, Monstrous Raider",
            6,
            6,
            VORINCLEX_ORACLE,
        );
        scenario.add_creature_from_oracle(
            P0,
            "Doc Samson, Super Psychiatrist",
            3,
            6,
            DOC_SAMSON_ORACLE,
        );
        let aboroth = scenario
            .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
            .id();
        let mut runner = scenario.build();

        runner.auto_advance_to_main_phase();
        runner.advance_until_stack_empty();

        // CR 702.24a + CR 616.1: the AGE counter placement is itself modified by
        // both permanents, so the very first prompt is the ordering choice.
        // Answering it with Vorinclex first makes the age total 1 → 2 → 3.
        assert_replacement_choice_between_vorinclex_and_doc_samson(
            &runner,
            "the age-counter placement must raise the CR 616.1 ordering prompt",
        );
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("ordering the age-counter replacements must be legal");
        runner.advance_until_stack_empty();

        // Reach guard: the cost really is the `EffectCost` shape (the second park
        // site), and it scaled with the modified age total.
        match &runner.state().waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(
                    *player, P0,
                    "the cumulative-upkeep payer is Aboroth's controller"
                );
                assert!(
                    matches!(
                        cost,
                        AbilityCost::EffectCost { effect } if matches!(
                            effect.as_ref(),
                            Effect::PutCounter {
                                counter_type: CounterType::Minus1Minus1,
                                count: QuantityExpr::Fixed { value: 3 },
                                target: TargetFilter::SelfRef,
                            }
                        )
                    ),
                    "the modified age total must scale the effect cost to three -1/-1 counters, got {cost:?}"
                );
            }
            other => panic!("expected Aboroth's cumulative-upkeep payment prompt, got {other:?}"),
        }

        runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("choosing to pay Aboroth's counter cost must be legal");

        // CR 616.1: the payment itself parks mid-cost on a replacement-ordering
        // choice. This is the assertion that proves the `EffectCost` park site is
        // reached at all.
        assert!(
            runner.state().pending_cost_move_resume.is_some(),
            "paying the effect cost must park the unless-payment continuation"
        );
        assert_replacement_choice_between_vorinclex_and_doc_samson(
            &runner,
            "the payment's own counter placement must raise the CR 616.1 ordering prompt",
        );

        let result = runner
            .act(GameAction::ChooseReplacement { index })
            .expect("ordering the payment's replacements must be legal");

        // CR 118.12: the resume settles through the PAID epilogue, so the
        // cumulative-upkeep ability finishes resolving and the whole reducer
        // step's event buffer survives. Membership, not order: at index 1 the two
        // `ReplacementApplied` events arrive Doc Samson first.
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == aboroth
            )),
            "a paid cumulative upkeep must emit the guarded ability's EffectResolved, got {:?}",
            result.events
        );
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::CounterAdded {
                    object_id,
                    counter_type: CounterType::Minus1Minus1,
                    count,
                    ..
                } if *object_id == aboroth && *count == expected_minus_counters
            )),
            "the counters the payer actually paid with must reach the event log, got {:?}",
            result.events
        );

        runner.advance_until_stack_empty();

        let aboroth_object = runner
            .state()
            .objects
            .get(&aboroth)
            .expect("Aboroth remains a known object");
        assert_eq!(
            aboroth_object.zone,
            Zone::Battlefield,
            "CR 118.12: a paid cumulative upkeep must not sacrifice the permanent"
        );
        assert_eq!(
            aboroth_object.counters.get(&CounterType::Minus1Minus1),
            Some(&expected_minus_counters),
            "ordering index {index} must settle the modified counter total"
        );
        assert_eq!(
            aboroth_object.counters.get(&CounterType::Age),
            Some(&3),
            "the age placement was modified by both replacements (1 → 2 → 3)"
        );
    }
}

/// Reach guard shared by both CR 616.1 prompts in the row above: the prompt is
/// the payer's, and it names both printed replacement sources. Asserted by
/// MEMBERSHIP rather than by index, because the candidate order differs between
/// the two prompts — an ordering drift must fail loudly, not silently re-index.
fn assert_replacement_choice_between_vorinclex_and_doc_samson(
    runner: &engine::game::scenario::GameRunner,
    context: &str,
) {
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        ref candidates,
    } = runner.state().waiting_for
    else {
        panic!("{context}, got {:?}", runner.state().waiting_for);
    };
    assert_eq!(
        player, P0,
        "{context}: the affected permanent's controller chooses"
    );
    assert_eq!(
        candidate_count, 2,
        "{context}: both replacements are candidates"
    );
    let names: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.source_name.as_str())
        .collect();
    assert!(
        names.contains(&"Vorinclex, Monstrous Raider"),
        "{context}: Vorinclex must be a candidate, got {names:?}"
    );
    assert!(
        names.contains(&"Doc Samson, Super Psychiatrist"),
        "{context}: Doc Samson must be a candidate, got {names:?}"
    );
}

/// CR 614.17b + CR 702.24a: the object-counter leg of "a player can't choose to
/// pay a cost that includes an event that can't happen".
///
/// Aboroth's cumulative upkeep is an `AbilityCost::EffectCost` whose payment
/// puts a -1/-1 counter on Aboroth itself. Solemnity's second sentence —
/// "Counters can't be put on artifacts, creatures, enchantments, or lands." —
/// is a CR 614.17 can't-effect on exactly that placement, so the pay branch is
/// never offered and CR 702.24a's "sacrifice it" happens instead.
///
/// Revert probe: without the choice-time refusal the prompt is
/// `WaitingFor::UnlessPayment { cost: EffectCost { PutCounter M1M1 SelfRef } }`
/// and Aboroth stays on the battlefield, failing both the `Priority` assertion
/// and the `Zone::Graveyard` assertion.
#[test]
fn aboroth_with_an_age_counter_under_solemnity_cannot_choose_to_pay() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY_ORACLE);
    let aboroth = scenario
        .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
        .id();
    // Control: an ordinary creature with no cumulative upkeep. The mechanism
    // under test cannot sacrifice it, so it must NOT reach the graveyard.
    let control = scenario.add_creature(P0, "Control Bear", 2, 2).id();
    // Without a library the suppressed prompt lets the turn run into the Draw
    // step, P0 decks out, and EVERY P0 object is exiled — which satisfies a
    // `!= Battlefield` assertion for the control creature too.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aboroth)
        .expect("Aboroth exists")
        .counters
        .insert(CounterType::Age, 1);

    // Aboroth's upkeep cost only materialises at >= 1 age counter.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the prohibited pay branch must leave no unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        !legal.is_empty(),
        "the action vector must still be live, got {legal:?}"
    );
    assert!(
        !legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "paying a -1/-1 counter cost must not be legal under Solemnity, got {legal:?}"
    );

    // CR 702.24a: an unpaid cumulative upkeep sacrifices the permanent.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&aboroth)
            .map(|obj| obj.zone)
            .expect("Aboroth object still exists"),
        Zone::Graveyard,
        "an unpayable cumulative upkeep must sacrifice Aboroth"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&control)
            .map(|obj| obj.zone)
            .expect("the control creature still exists"),
        Zone::Battlefield,
        "the control creature has no cumulative upkeep and must not be sacrificed"
    );
}

/// The quantity used by the two CR 107.1b rows below: `HandSize(controller) - 2`,
/// which resolves to -2 while the payer's hand is empty.
///
/// `QuantityExpr::Offset` is the shape chosen over `Multiply { factor: -1, .. }`
/// because it is the one the resolver can drive negative from ordinary board
/// state: `fold_compose` evaluates it as an unfloored `inner + offset`, so any
/// cost quantity whose dynamic inner falls below its offset arrives at the
/// payment negative. Negative-offset `Offset` nodes are ordinary parser output
/// ("X minus one"); a negative `Multiply` factor is real too, but every producer
/// of one is a power/toughness or life-direction sign, never a counter count.
fn hand_size_minus_two() -> QuantityExpr {
    QuantityExpr::Offset {
        inner: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        }),
        offset: -2,
    }
}

/// Replaces the counter count inside `source`'s OWN synthesized cumulative-upkeep
/// cost, leaving every other part of the production trigger — the age-counter
/// tick, the `PerCounter` wrapper, the payer, the sacrifice branch — exactly as
/// the trigger synthesizer emitted it. The prompt, the choice-time payability
/// answer and the payment therefore all come from production code; only the leaf
/// quantity is fixture data.
///
/// The quantity is SYNTHETIC, stated as a predicate rather than as a card list:
/// no printed card's counter cost resolves to a negative number, so the value has
/// to be installed. The rows below pin the rules-correct answer for the shape,
/// not the behavior of any printed card.
///
/// The `expect` is a reach guard: if synthesis stops emitting this cost shape the
/// rows fail loudly instead of quietly pinning nothing.
fn install_cumulative_upkeep_count(runner: &mut GameRunner, source: ObjectId, count: QuantityExpr) {
    let object = runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("the cumulative-upkeep source exists");
    let slot = Arc::make_mut(&mut object.base_trigger_definitions)
        .iter_mut()
        .filter_map(|trigger| trigger.execute.as_mut())
        .filter_map(|execute| execute.sub_ability.as_mut())
        .filter_map(|branch| branch.unless_pay.as_mut())
        .find_map(|unless| match &mut unless.cost {
            AbilityCost::PerCounter { base, .. } => match base.as_mut() {
                AbilityCost::EffectCost { effect } => match effect.as_mut() {
                    Effect::PutCounter { count, .. } => Some(count),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .expect("the synthesized cumulative upkeep carries a per-counter PutCounter effect cost");
    *slot = count;
    object.materialize_base_trigger_definitions();
}

/// Asserts that the live unless-payment prompt is the payer's and carries
/// `expected` as its counter-cost quantity — the reach guard both rows below
/// need, because a prompt carrying a different quantity would make everything
/// downstream of it prove nothing.
fn assert_unless_payment_carries_count(runner: &GameRunner, expected: &QuantityExpr) {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(
                *player, P0,
                "the cumulative-upkeep payer is the permanent's controller"
            );
            assert!(
                matches!(
                    cost,
                    AbilityCost::EffectCost { effect } if matches!(
                        effect.as_ref(),
                        Effect::PutCounter {
                            counter_type: CounterType::Minus1Minus1,
                            count,
                            target: TargetFilter::SelfRef,
                        } if count == expected
                    )
                ),
                "the unless-payment cost must carry the installed quantity, got {cost:?}"
            );
        }
        other => panic!("expected the cumulative-upkeep payment prompt, got {other:?}"),
    }
}

/// CR 107.1b: a counter cost whose quantity resolves NEGATIVE places ZERO
/// counters, not its magnitude.
///
/// "If a calculation that would determine the result of an effect yields a
/// negative number, zero is used instead, unless that effect doubles, triples,
/// or sets to a specific value a player's life total or the power and/or
/// toughness of a creature or creature card." A counter count is in none of
/// those exception classes, so -2 is simply 0.
///
/// The whole path is production: the synthesized cumulative-upkeep trigger
/// emits `WaitingFor::UnlessPayment`, `PayUnlessCost { pay: true }` goes through
/// `apply()`, and the payment resolves the quantity itself. Only the leaf
/// quantity is installed — see `install_cumulative_upkeep_count`, which also
/// records why the shape is synthetic.
///
/// Revert probe: reading the resolved quantity as `resolved.unsigned_abs()`
/// instead of clamping it at zero makes the payment place TWO -1/-1 counters,
/// failing the last assertion. The three assertions above it hold in BOTH
/// directions on purpose — they are reach guards, not discriminators.
#[test]
fn cumulative_upkeep_negative_counter_cost_quantity_places_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let aboroth = scenario
        .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
        .id();
    let mut runner = scenario.build();
    install_cumulative_upkeep_count(&mut runner, aboroth, hand_size_minus_two());

    // Control on the fixture's own premise: the installed quantity is negative
    // only because the payer's hand is empty when the upkeep resolves. Asserting
    // it here makes a future scenario default that deals an opening hand fail
    // loudly rather than silently turn the cost positive.
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "the installed quantity resolves to -2 only while the payer's hand is empty"
    );

    // No age counter is pre-installed, so the upkeep tick makes exactly one and
    // the per-counter expansion's x1 scaling returns the quantity unchanged.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    // Reach guard (i): the negative quantity is what the production prompt
    // actually carries into the payment.
    assert_unless_payment_carries_count(&runner, &hand_size_minus_two());

    // Reach guard (ii): CR 614.17b did not refuse the choice. A zero-counter cost
    // includes no counter-placement event, so there is nothing for a can't-effect
    // to forbid and the pay branch is offered.
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a cost that places no counters must remain choosable");
    runner.advance_until_stack_empty();

    let aboroth_object = runner
        .state()
        .objects
        .get(&aboroth)
        .expect("Aboroth remains a known object");
    // Reach guard (iii): the cost was PAID, not declined. CR 702.24a sacrifices a
    // permanent whose cumulative upkeep goes unpaid, so surviving on the
    // battlefield with the ticked age counter is what proves the payment ran at
    // all — without it the counter assertion below would pass vacuously.
    assert_eq!(
        aboroth_object.zone,
        Zone::Battlefield,
        "a paid cumulative upkeep must not sacrifice Aboroth"
    );
    assert_eq!(
        aboroth_object.counters.get(&CounterType::Age),
        Some(&1),
        "the upkeep tick places exactly one age counter"
    );

    // CR 107.1b: the discriminator. `unsigned_abs()` places two counters here.
    assert_eq!(
        aboroth_object
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        0,
        "a counter cost resolving to -2 must place zero counters, not two"
    );
}

/// CR 614.17b + CR 107.1b: because a NEGATIVE counter cost places zero counters,
/// a counter prohibition must not make it unchoosable.
///
/// "If an event can't happen, a player can't choose to pay a cost that includes
/// that event." A zero-counter cost includes no counter-placement event —
/// `preview_counter_addition` answers `count == 0` with `Applied { count: 0 }`
/// before consulting the replacement pipeline — so Solemnity has nothing to
/// forbid and the pay branch stays on the table.
///
/// The board is held identical to
/// `aboroth_with_an_age_counter_under_solemnity_cannot_choose_to_pay`, whose
/// POSITIVE count makes the same Solemnity refusal CORRECT and sacrifices
/// Aboroth. Moving only the quantity is what makes this row about the count
/// rather than about Solemnity.
///
/// That identical board includes the PRE-INSTALLED age counter, which this row
/// needs to reach the payment at all: Solemnity prevents the upkeep's own
/// age-counter tick, so on a board with none the per-counter expansion runs at
/// n = 0 and CR 702.24a's "for each age counter on it" leaves no cost instance
/// to pay — the unless-effect resolves with no prompt and the count under test
/// is never reached. The age assertion below pins that mechanism rather than
/// leaving it to a comment.
///
/// One assertion is deliberately absent. "Aboroth has no -1/-1 counters" is
/// vacuous on this board: Solemnity would prevent the placement anyway, and the
/// shared add primitive reports a prevented placement as complete, so that
/// assertion passes with the clamp and without it. The discriminators are the
/// three assertions that the CHOICE survives.
///
/// Revert probe: reading the resolved quantity as `resolved.unsigned_abs()`
/// makes the choice-time predicate preview a TWO-counter placement, Solemnity
/// forbids it, the prompt is suppressed, and Aboroth is sacrificed. The row
/// aborts at the age-counter guard, which goes red because CR 122.2 cleared
/// that counter with the sacrifice rather than because the tick ran; the three
/// discriminators below it are unreachable in that direction.
#[test]
fn cumulative_upkeep_negative_counter_cost_quantity_stays_choosable_under_solemnity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY_ORACLE);
    let aboroth = scenario
        .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
        .id();
    // A suppressed prompt would let the turn run into the Draw step; without a
    // library P0 decks out and every P0 object is exiled, which would blur the
    // reverted-direction failure. The library keeps that failure legible.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    let mut runner = scenario.build();
    install_cumulative_upkeep_count(&mut runner, aboroth, hand_size_minus_two());
    // CR 702.24a: the upkeep cost only materialises at >= 1 age counter, and
    // Solemnity prevents the tick that would otherwise place the first one.
    runner
        .state_mut()
        .objects
        .get_mut(&aboroth)
        .expect("Aboroth exists")
        .counters
        .insert(CounterType::Age, 1);

    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "the installed quantity resolves to -2 only while the payer's hand is empty"
    );

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    // The mechanism this row rests on, asserted rather than assumed: Solemnity
    // prevents the upkeep's own age-counter tick, so the total stays at the
    // pre-installed 1 and the cost materialises from that one counter. A 2 here
    // would mean the tick was not prevented and the cost scaled, which would make
    // the prompt guard below assert against the wrong quantity.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&aboroth)
            .and_then(|object| object.counters.get(&CounterType::Age)),
        Some(&1),
        "Solemnity must prevent the age tick, leaving the pre-installed counter as the whole cost"
    );

    // CR 614.17b, discriminator (i): the prompt is emitted at all. With a positive
    // count on this exact board it is not — that is the sibling row.
    assert_unless_payment_carries_count(&runner, &hand_size_minus_two());

    // CR 614.17b, discriminator (ii): the pay branch is offered.
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "a cost that places no counters includes no prohibited event, so paying \
         must stay legal under Solemnity, got {legal:?}"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a cost that places no counters must remain choosable under Solemnity");
    runner.advance_until_stack_empty();

    // CR 702.24a, discriminator (iii): the cost was payable and paid, so the
    // guarded "sacrifice it" never happens.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&aboroth)
            .map(|object| object.zone)
            .expect("Aboroth remains a known object"),
        Zone::Battlefield,
        "a choosable, paid cumulative upkeep must not sacrifice Aboroth"
    );
}
