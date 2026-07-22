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
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
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
