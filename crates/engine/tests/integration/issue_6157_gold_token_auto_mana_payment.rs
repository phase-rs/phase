//! Issue #6157: Gold tokens are not used by automatic mana payment.
//!
//! Gold's mana ability ("Sacrifice this token: Add one mana of any color.",
//! CR 111.10c) has a bare `Sacrifice` cost with no `{T}` component. Auto-tap
//! source discovery (`mana_sources::is_active_tap_mana_ability`) required a
//! `{T}` cost component on every scanned ability, so Gold was invisible to
//! `CastPaymentMode::Auto` even though its cost sacrifices only the token
//! itself and needs no player choice — exactly as deterministic as a `{T}`
//! cost. Treasure worked only because its cost happens to also include `{T}`
//! (`{T}, Sacrifice this artifact: ...`).
//!
//! Fix: auto-tap source discovery now also accepts an unambiguous
//! self-sacrifice cost (`Sacrifice` targeting only the source, count 1).

use engine::game::effects::token::predefined_token_abilities;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, DiscardSelfScope, Effect,
    ManaContribution, ManaProduction, QuantityExpr, SacrificeCost, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn make_token(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    subtype: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        subtype.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    let abilities = predefined_token_abilities(subtype);
    *std::sync::Arc::make_mut(&mut obj.abilities) = abilities.clone();
    *std::sync::Arc::make_mut(&mut obj.base_abilities) = abilities;
    id
}

fn draw_spell(scenario: &mut GameScenario) -> ObjectId {
    scenario.with_library_top(P0, &["Filler Card"]);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Auto-Pay Draw", true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id()
}

#[test]
fn gold_token_is_auto_tapped_for_mana_like_treasure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let gold = make_token(runner.state_mut(), 900, "Gold");

    let outcome = runner.cast(spell).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should fully resolve using the Gold token without pausing for manual input, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[gold], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
}

#[test]
fn treasure_token_is_auto_tapped_for_mana_control_case() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let treasure = make_token(runner.state_mut(), 901, "Treasure");

    let outcome = runner.cast(spell).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should fully resolve using the Treasure token without pausing for manual input, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[treasure], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
}

/// Regression for the maintainer review on PR #6230: a Gold token that is
/// already **tapped** (and summoning-sick) must still be selected by the
/// auto-tap payment path. Gold's cost is "Sacrifice this token: Add one mana
/// of any color." — an unambiguous self-sacrifice with **no** `{T}` component
/// (CR 111.10c). CR 106.12 / CR 302.6 gate only `{T}`/`{Q}` costs on an
/// untapped, non-summoning-sick source, so a tapped Gold can legally pay a
/// sacrifice cost. The earlier object-level tapped/summoning-sickness prefilter
/// short-circuited before the self-sacrifice predicate could run, leaving the
/// spell unpayable; this test fails on that pre-fix code and passes once the
/// prefilter is made conditional on the ability requiring `{T}`.
#[test]
fn tapped_gold_token_still_auto_taps_for_self_sacrifice_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let gold = make_token(runner.state_mut(), 902, "Gold");
    // Tap the Gold token and mark it summoning-sick: neither state may block a
    // sacrifice-cost mana ability, since the cost carries no `{T}` symbol.
    {
        let obj = runner.state_mut().objects.get_mut(&gold).unwrap();
        obj.tapped = true;
        obj.summoning_sick = true;
    }

    let outcome = runner.cast(spell).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should fully resolve using a tapped Gold token via its \
         self-sacrifice mana ability without pausing for manual input, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[gold], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
}

/// Builds a permanent whose only mana ability is Lion's Eye Diamond-shaped:
/// `Composite[Discard{ selection: Chosen }, Sacrifice(SelfRef, 1)]` — "Discard
/// your hand, Sacrifice this artifact: Add one mana of any color." (matches the
/// LED build in `mana_abilities.rs`).
///
/// The `Sacrifice(SelfRef, 1)` sibling looks like Gold's unambiguous
/// self-sacrifice, but the `Discard{Chosen}` sibling forces a player choice, so
/// the *whole* cost is NOT choice-free. LED must therefore stay on the
/// manual-payment path (its discard prompt must never be silently bypassed) and
/// must NOT be selected by auto-tap.
fn make_led_source(state: &mut engine::types::game_state::GameState, card_id: u64) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Lion Eye Diamond".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    let led_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![
                    ManaColor::White,
                    ManaColor::Blue,
                    ManaColor::Black,
                    ManaColor::Red,
                    ManaColor::Green,
                ],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand,
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    });
    let abilities = vec![led_ability];
    *std::sync::Arc::make_mut(&mut obj.abilities) = abilities.clone();
    *std::sync::Arc::make_mut(&mut obj.base_abilities) = abilities;
    id
}

/// End-to-end regression for the maintainer review on PR #6230: prove the
/// Lion's Eye Diamond-shaped composite cost
/// (`Composite[Discard{Chosen}, Sacrifice(SelfRef, 1)]`) **remains on the
/// manual-payment path** — auto-tap must never select it — while a bare Gold
/// token (unambiguous self-sacrifice) stays eligible.
///
/// This drives the same `CastPaymentMode::Auto` entry point as the three tests
/// above (`runner.cast(spell).resolve()`), not just the
/// `has_unambiguous_self_sacrifice_component` predicate. The discriminating
/// board is **LED + Gold together**: auto-tap must skip LED (its `Discard`
/// sibling needs a player choice) and pay the generic-1 spell with Gold, so the
/// spell resolves, Gold is sacrificed, and the LED source is left untouched on
/// the battlefield.
///
/// On the pre-fix `any`-match predicate LED is misclassified as an unambiguous
/// self-sacrifice source, so auto-tap discovery admits it into the payment plan;
/// its `Discard` cost then cannot be satisfied by the auto path and the whole
/// payment is rejected — the otherwise-payable spell (Gold is right there)
/// becomes unpayable. This test fails on that pre-fix code (`try_resolve`
/// returns `Err`) and passes once the whole-tree choice-free check keeps LED off
/// the auto-tap path.
#[test]
fn led_shaped_discard_sacrifice_not_auto_tapped_stays_manual() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    // Cards in hand so LED's Discard cost is *satisfiable* — this rules out
    // "LED failed only because the hand was empty" and isolates the auto-tap
    // classification as the cause.
    scenario.add_card_to_hand(P0, "Spare Card A");
    scenario.add_card_to_hand(P0, "Spare Card B");
    let mut runner = scenario.build();
    let led = make_led_source(runner.state_mut(), 950);
    let gold = make_token(runner.state_mut(), 951, "Gold");

    let outcome = runner
        .cast(spell)
        .try_resolve()
        .expect("auto-tap must pay the generic-1 spell with Gold and skip the LED-shaped source");

    // Auto-tap resolved the spell using Gold; it must not have paused for input.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should resolve via the Gold token without pausing, got {:?}",
        outcome.final_waiting_for()
    );
    // Gold paid (sacrificed); the spell resolved and drew.
    outcome.assert_zone(&[gold], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    // The LED-shaped source was NOT auto-tapped: it stays on the battlefield and
    // no discard was forced through it.
    let led_zone = outcome.state().objects.get(&led).map(|o| o.zone);
    assert_eq!(
        led_zone,
        Some(Zone::Battlefield),
        "the LED-shaped composite source must remain on the battlefield \
         (manual-payment path), never auto-tapped for the spell"
    );

    // Sanity floor: with **only** the LED-shaped source available (no Gold), the
    // spell is not auto-payable at all — auto-tap has nothing choice-free to use,
    // confirming LED itself never covers the cost on the auto path.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    scenario.add_card_to_hand(P0, "Spare Card A");
    let mut runner = scenario.build();
    let led = make_led_source(runner.state_mut(), 952);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting with only the LED-shaped source must reach manual mana payment");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player, .. } if player == P0
        ),
        "with only the LED-shaped source, auto payment must pause at manual mana payment, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cast.as_deref(),
            Some(pending) if pending.object_id == spell
        ),
        "the manual mana-payment pause must preserve the pending spell cast"
    );
    assert_eq!(
        runner.state().objects.get(&led).map(|object| object.zone),
        Some(Zone::Battlefield),
        "the LED-shaped source must remain available for its manual discard-and-sacrifice payment"
    );
}
