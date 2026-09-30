//! Issue #782 — Thought Lash: "Exile the top card of your library: Prevent the
//! next 1 damage that would be dealt to you this turn."
//!
//! Reported as the activated ability resolving without its exile-the-top-card
//! cost being paid. CR 602.2b + CR 601.2h: activating an ability pays its total
//! cost, and an ability whose cost can't be paid can't be activated.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef,
    ReplacementDefinition, ReplacementMode, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

// Verbatim Oracle text (Scryfall, 2026-09-15).
const THOUGHT_LASH: &str = "Cumulative upkeep—Exile the top card of your library. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)\nWhen a player doesn't pay this enchantment's cumulative upkeep, that player exiles all cards from their library.\nExile the top card of your library: Prevent the next 1 damage that would be dealt to you this turn.";

fn thought_lash_with_library(library_top_first: &[&str]) -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    if !library_top_first.is_empty() {
        scenario.with_library_top(P0, library_top_first);
    }
    let lash = scenario
        .add_creature(P0, "Thought Lash", 0, 0)
        .as_enchantment()
        .from_oracle_text(THOUGHT_LASH)
        .id();
    let runner = scenario.build();
    // Reach-guard: the activated ability exists and its cost is the
    // top-of-library exile, so every later assertion is about paying it.
    let index = runner.state().objects[&lash]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &ability.cost,
                Some(AbilityCost::Exile {
                    zone: Some(Zone::Library),
                    ..
                })
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "Thought Lash must carry an exile-the-top-card activated ability; got {:?}",
                runner.state().objects[&lash]
                    .abilities
                    .iter()
                    .map(|a| (&a.cost, &a.effect))
                    .collect::<Vec<_>>()
            )
        });
    (runner, lash, index)
}

fn library_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[0].library.iter().copied().collect()
}

#[test]
fn thought_lash_activation_exiles_the_top_card_of_your_library() {
    let (mut runner, lash, index) =
        thought_lash_with_library(&["Top Card", "Second Card", "Third Card"]);
    let before = library_ids(&runner);
    let top = *before.first().expect("reach-guard: library seeded");

    runner
        .act(GameAction::ActivateAbility {
            source_id: lash,
            ability_index: index,
        })
        .expect("activation with a nonempty library must be accepted");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation settles at priority; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "paying the cost exiles the top card of the library"
    );
    assert_eq!(
        library_ids(&runner),
        before[1..].to_vec(),
        "exactly the top card leaves the library"
    );
}

#[test]
fn thought_lash_cannot_be_activated_with_an_empty_library() {
    let (mut runner, lash, index) = thought_lash_with_library(&[]);
    assert!(
        library_ids(&runner).is_empty(),
        "reach-guard: the library is empty"
    );
    let stack_before = runner.state().stack.len();

    let result = runner.act(GameAction::ActivateAbility {
        source_id: lash,
        ability_index: index,
    });

    assert!(
        result.is_err() || runner.state().stack.len() == stack_before,
        "an unpayable exile-the-top-card cost must not put the ability on the stack; \
         result = {result:?}, stack = {:?}",
        runner.state().stack
    );
}

// Verbatim Oracle text (Scryfall, 2026-09-16).
const PHYREXIAN_DEVOURER: &str = "When this creature's power is 7 or greater, sacrifice it.\nExile the top card of your library: Put X +1/+1 counters on this creature, where X is the exiled card's mana value.";

/// Find the exile-the-top-card activated ability on `source`.
fn exile_top_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &ability.cost,
                Some(AbilityCost::Exile {
                    zone: Some(Zone::Library),
                    ..
                })
            )
        })
        .expect("reach-guard: the exile-the-top-card activated ability must be present")
}

/// CR 608.2k: "If an ability's effect refers to a specific untargeted object
/// that has been previously referred to by that ability's COST ... it still
/// affects that object." Phyrexian Devourer's "where X is the exiled card's
/// mana value" names the card its own cost exiled, so the cost-paid referent
/// must be bound BEFORE the payment moves that card to exile.
///
/// Revert-proof: without the pre-payment binding, X resolves against no
/// referent and the Devourer gains no counters (stays 1/1).
#[test]
fn phyrexian_devourer_counts_the_exiled_cards_mana_value() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Three Drop", "Filler One", "Filler Two"]);

    let devourer = {
        let mut b = scenario.add_creature(P0, "Phyrexian Devourer", 1, 1);
        b.from_oracle_text(PHYREXIAN_DEVOURER);
        b.id()
    };

    let mut runner = scenario.build();

    // A known mana value on top so X is unambiguous: {2}{R} = 3.
    let top = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");
    runner.state_mut().objects.get_mut(&top).unwrap().mana_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };

    let index = exile_top_ability_index(&runner, devourer);
    runner
        .act(GameAction::ActivateAbility {
            source_id: devourer,
            ability_index: index,
        })
        .expect("activation must be accepted with a nonempty library");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "reach-guard: the cost exiled the top card"
    );
    assert_eq!(
        (
            runner.state().objects[&devourer].power,
            runner.state().objects[&devourer].toughness
        ),
        (Some(4), Some(4)),
        "CR 608.2k: X must equal the exiled card's mana value (3), leaving a 4/4"
    );
}

/// CR 400.7 + CR 608.2k: an activation whose cost moves a card, and whose effect
/// then AFFECTS that same card, must resolve the cost-paid referent to the card's
/// CURRENT incarnation — the one the cost's own move produced.
///
/// `CostPaidObjectSnapshot::live_object_id` is the single authority for resolving
/// that referent to a live object, and it yields `None` as soon as the recorded
/// incarnation stops matching. The binding seams necessarily capture BEFORE the
/// move (their `lki` must hold pre-move characteristics — CR 608.2h), so without a
/// post-payment re-pin the DIRECT activation path leaves the pin one incarnation
/// behind and every live-object consumer silently affects nothing. The
/// replacement-paused path already re-pins at its completion
/// (`casting_costs::finish_cost_object_moves`); this covers the direct path.
///
/// The ability is constructed because no printed card reaches this combination: of
/// the five cards whose activation cost exiles from the top of the library (Thought
/// Lash, Phyrexian Devourer, Royal Herbalist, Storm Elemental, Whirling Catapult)
/// none AFFECTS the card its own cost exiled. Phyrexian Devourer only reads that
/// card's mana value, which CR 608.2h serves from the frozen LKI, so it passes with
/// or without the re-pin and cannot discriminate. Everything beneath the ability is
/// production: a real `GameAction::ActivateAbility`, the real deterministic
/// library-exile payment, and the real `Effect::PutCounter` resolver.
///
/// Stays on the direct path by construction: `TargetFilter::CostPaidObject` is a
/// context ref (`TargetFilter::is_context_ref`), so it claims no target slot and
/// the activation never detours through target selection.
///
/// Revert-proof: drop `resolved.repin_cost_paid_object_recursive(state)` from
/// `casting::handle_activate_ability` and the exiled card receives NO counter,
/// because `live_object_id` returns `None` against the stale pre-move pin.
#[test]
fn a_direct_activation_affects_the_card_its_own_cost_exiled() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Paid Card", "Filler One", "Filler Two"]);
    let source = scenario.add_creature(P0, "Counter Stamper", 2, 2).id();

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::CostPaidObject,
        },
    )
    .cost(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Library),
        filter: None,
    });
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source object exists");
        // Every layer pass resets `abilities` from `base_abilities`, so set both —
        // assigning only the derived field lets a flush before activation erase the
        // ability and turn an engine result into a harness artifact.
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let top = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");

    // Reach-guard: the constructed ability survived to activation time with its
    // library-exile cost intact, so neither assertion below can pass vacuously.
    let index = exile_top_ability_index(&runner, source);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .expect("a deterministic library-exile activation must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "reach-guard: the cost exiled the top card"
    );
    assert_eq!(
        runner.state().objects[&top]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "CR 608.2k: the effect must affect the very card its own cost exiled; a \
         stale cost-paid pin resolves to no live object and places nothing"
    );
}

/// CR 608.2k + CR 400.7: a TARGETFUL activation pays its cost through the
/// target-first boundary (`casting_costs::push_activated_ability_to_stack`), never the
/// direct path — so that boundary must apply the SAME two cost-paid authorities the
/// direct path does: bind the referent before the cost moves it, then re-pin once the
/// move is complete.
///
/// It did neither. The seam stamped only the self-discard binding, so an activation
/// whose deterministic top-of-library exile cost is referred to by its own effect had
/// no referent at all; and once bound, the pin still named the pre-move incarnation,
/// so `CostPaidObjectSnapshot::live_object_id` yielded `None` against the object the
/// cost itself had just moved.
///
/// No printed card reaches this combination. Of the five cards whose activation cost
/// exiles from the top of the library, Storm Elemental is the only one with a
/// *targetful* such ability ("Tap target creature with flying") and it never refers
/// back to the exiled card. The ability is therefore constructed, but the route is
/// entirely production: a real `GameAction::ActivateAbility`, a real
/// `WaitingFor::TargetSelection` answered by `GameAction::SelectTargets`, the real
/// deterministic library-exile payment, and the real `Effect::Destroy` +
/// `Effect::PutCounter` resolvers.
///
/// Two creatures are on the battlefield so target selection cannot auto-resolve a
/// single legal target and silently take a different route; the reach-guard below
/// pins that this activation really did pause on target selection.
///
/// Revert-proof: drop either the `stamp_top_library_exile_cost_paid_object` call or
/// the `repin_cost_paid_object_recursive` call from
/// `casting_costs::push_activated_ability_to_stack` and the exiled card receives no
/// counter (`None` instead of `Some(1)`), while the destroy half still succeeds — so
/// the assertion isolates the cost-paid binding, not the effect as a whole.
#[test]
fn a_targetful_activation_affects_the_card_its_own_cost_exiled() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Paid Card", "Filler One", "Filler Two"]);
    let source = scenario.add_creature(P0, "Targeting Stamper", 2, 2).id();
    let victim = scenario.add_creature(P1, "Victim Bear", 2, 2).id();

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            cant_regenerate: false,
        },
    )
    .cost(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Library),
        filter: None,
    })
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::CostPaidObject,
        },
    ));
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source object exists");
        // Every layer pass resets `abilities` from `base_abilities`, so set both.
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let top = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");
    let index = exile_top_ability_index(&runner, source);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .expect("a targetful library-exile activation must be accepted");

    // Reach-guard: this is genuinely the TARGET-FIRST route, not the direct path
    // (which a separate test covers) — otherwise the assertion below would prove
    // nothing about this seam.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach-guard: the activation must pause on real target selection; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(victim)],
        })
        .expect("choosing the creature to destroy must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "reach-guard: the cost exiled the top card"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach-guard: the targeted half of the ability resolved"
    );
    assert_eq!(
        runner.state().objects[&top]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "CR 608.2k: the target-first path must BIND and RE-PIN the cost-paid \
         referent, so the rider affects the very card the cost exiled"
    );
}

/// CR 118.11: "The actions performed when paying a cost may be modified by effects.
/// Even if they are ... the cost has still been paid." So a deterministic
/// top-of-library exile cost publishes the count it CALLED FOR, and a replacement
/// that interrupts the payment must not change that number.
///
/// An unpaused payment publishes the count inline in `costs::pay_ability_cost_inner`.
/// A payment that pauses returns BEFORE that write, and its resume previously only
/// re-pinned and finished — so `QuantityRef::EventContextAmount` found nothing and
/// bottomed out at `unwrap_or(0)`. The count now rides the round trip on
/// `PendingCostMoveResume::Cast::requested_cost_count` and is published at the
/// completion, once the paused object and every remaining leg have settled.
///
/// Two cards, deliberately: the redirect pauses the payment TWICE, so this also
/// covers the re-park inside `finish_cost_object_moves` carrying the owed count
/// across a SECOND pause. A one-card cost cannot reach that path (its resume starts
/// past the end of `chosen`), and Whirling Catapult — "exile the top two cards of
/// your library" — is exactly this shape.
///
/// The count is read directly, and deliberately so. `last_effect_count` is cleared
/// at the START of every player action (`engine::apply_action_boundary_core`: "clear
/// transient inter-effect state"), and it is consumed by `sub_ability` continuations
/// within the SAME action. An activated ability pays its cost in one action and
/// resolves in a later one, so its own effect can never observe a count published by
/// its own cost — with or without this fix. The observable point is therefore right
/// after the final replacement answer completes the payment, before any further
/// action clears it; that is exactly where "continue the activation with stale or
/// absent `EventContextAmount`" bites.
///
/// Revert-proof: without the carried count this reads `None` while the cost still
/// exiles both called-for cards — so the assertion isolates the published count, not
/// the payment.
#[test]
fn a_replacement_paused_library_exile_cost_still_publishes_its_requested_count() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Six cards: two leave as the cost, two are drawn, and the rest keep the
    // library non-empty so no draw-from-empty loss ends the game first.
    scenario.with_library_top(
        P0,
        &[
            "Paid One", "Paid Two", "Draw One", "Draw Two", "Spare A", "Spare B",
        ],
    );

    let source = scenario.add_creature(P0, "Count Publisher", 2, 2).id();
    scenario
        .add_creature(P0, "Optional Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(optional_exile_redirect());

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Exile {
        count: 2,
        zone: Some(Zone::Library),
        filter: None,
    });
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source object exists");
        // Layers reset `abilities` from `base_abilities` on every pass; set both.
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let paid: Vec<ObjectId> = runner.state().players[0]
        .library
        .iter()
        .copied()
        .take(2)
        .collect();
    assert_eq!(
        paid.len(),
        2,
        "reach-guard: library seeded with the paid cards"
    );
    let index = exile_top_ability_index(&runner, source);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .expect("a two-card library-exile activation must be accepted");

    // Decline the redirect each time it interrupts the payment, counting the
    // pauses. This count is the whole point of the fixture: if the payment never
    // paused, the INLINE publish in `costs.rs` would satisfy the assertion below
    // and the test would prove nothing about the resume path.
    let mut pauses = 0;
    for _ in 0..8 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        pauses += 1;
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("declining the optional redirect must be accepted");
    }
    // Read the published count HERE: the payment has just completed on the resume,
    // and the next player action would clear it (see this test's doc comment).
    let published = runner.state().last_effect_count;

    runner.advance_until_stack_empty();

    assert!(
        pauses >= 1,
        "reach-guard: the cost move must pause on the replacement at least once, \
         otherwise the inline publish would satisfy this test vacuously"
    );
    for card in &paid {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Exile,
            "reach-guard: both called-for cards were exiled to pay the cost"
        );
    }
    assert_eq!(
        published,
        Some(2),
        "CR 118.11: the cost called for two cards, so the paid count published to \
         `EventContextAmount` is 2 even though a replacement interrupted the payment"
    );
}

/// An OPTIONAL redirect of a move into `Zone::Exile`. Declining it leaves the
/// card settling in exile, which is the replacement-choice-to-exile path — the
/// shape `cost_zone_pipeline::exile_tracking_parked_resume_preserves_source_link`
/// uses for the effect-driven case.
fn optional_exile_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Graveyard,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .mode(ReplacementMode::Optional { decline: None })
}

/// CR 406.6 + CR 616.1: a deterministic library-exile ACTIVATION cost that pauses
/// on a replacement choice and then settles in exile must still index the paid
/// card as "exiled with [source] this turn".
///
/// Revert-proof: the cast cost-move resume re-enters `finish_cost_object_moves`
/// at `paused_at_index + 1`, so without recording the settled paused object at
/// the delivery boundary this single-card cost records no link at all.
#[test]
fn a_paused_activation_cost_that_settles_in_exile_keeps_its_source_link() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Paid Card", "Filler One", "Filler Two"]);

    let lash = scenario
        .add_creature(P0, "Thought Lash", 0, 0)
        .as_enchantment()
        .from_oracle_text(THOUGHT_LASH)
        .id();
    scenario
        .add_creature(P0, "Optional Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(optional_exile_redirect());

    let mut runner = scenario.build();
    let paid = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");
    let index = exile_top_ability_index(&runner, lash);

    runner
        .act(GameAction::ActivateAbility {
            source_id: lash,
            ability_index: index,
        })
        .expect("activation must be accepted");

    // Reach-guard: the cost move really did pause on the replacement choice —
    // otherwise this test would pass through the unpaused path and prove nothing.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the cost move must pause on the optional redirect; got {:?}",
        runner.state().waiting_for
    );

    // Decline the redirect, so the card settles in exile after the pause.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the optional redirect must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&paid].zone,
        Zone::Exile,
        "reach-guard: declining the redirect leaves the paid card in exile"
    );
    assert!(
        runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&lash)
            .is_some_and(|cards| cards.contains(&paid)),
        "CR 406.6: the paid card must be indexed as exiled with its source even \
         when its cost move paused on a replacement choice; index = {:?}",
        runner.state().cards_exiled_with_source_this_turn.get(&lash)
    );
}
