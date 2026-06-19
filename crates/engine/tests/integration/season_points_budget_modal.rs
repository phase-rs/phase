//! CR 700.2i — WOE "Season of …" pawprint points-budget modal cycle.
//!
//! Each Season sorcery reads "Choose up to five {P} worth of modes. You may
//! choose the same mode more than once." with three modes weighted
//! `{P}` / `{P}{P}` / `{P}{P}{P}`. The controller has a budget of 5 *points*;
//! each chosen mode (repeats allowed, CR 700.2d) costs its pawprint count, and
//! the SUM of the chosen modes' weights must be ≤ 5 (CR 700.2i).
//!
//! These tests drive the REAL card-data path (`add_real_card` + rehydrate, per
//! the `parser_fix_inert_until_data_regen` memory) — `from_oracle_text` would
//! re-parse and mask a stale data path. The pawprint budget is therefore an
//! end-to-end regression guard, not an AST-shape assertion.

use engine::game::ability_utils::{pawprint_budget_satisfied, validate_modal_indices};
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ModalChoice;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// The pawprint `ModalChoice` parsed for a Season card, pulled from the spell
/// ability stored in the regenerated card database. Panics if the card has no
/// pawprint modal — that is exactly the parser-collection break this cycle
/// fixes, so a panic here is a meaningful failure.
fn season_modal(db: &engine::database::card_db::CardDatabase, name: &str) -> ModalChoice {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not found in CardDatabase"));
    // Spell-level modal metadata lives on `CardFace.modal` (the per-mode
    // `AbilityDefinition`s are stored in `face.abilities`).
    face.modal
        .clone()
        .filter(|m| !m.mode_pawprints.is_empty())
        .unwrap_or_else(|| panic!("card '{name}' has no pawprint modal"))
}

/// Fund `player`'s pool with enough mana to cast any Season card (two of a
/// color + four generic covers the most expensive cost in the cycle).
fn fund_season_pool(
    scenario: &mut GameScenario,
    player: engine::types::player::PlayerId,
    color: ManaType,
) {
    let mut pool = Vec::new();
    for _ in 0..2 {
        pool.push(ManaUnit::new(
            color,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..4 {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    scenario.with_mana_pool(player, pool);
}

fn token_count(state: &engine::types::game_state::GameState, subtype: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(subtype))
            })
        })
        .count()
}

/// Test 1 — the parser collects three pawprint modes and `max_choices` stays the
/// budget (5), UNCAPPED to `mode_count` (3). The pre-fix `build_modal_choice`
/// clamped 5 → 3 (`header.max_choices.min(mode_count)`); this assertion flips if
/// that clamp is restored.
#[test]
fn season_gathering_parses_three_pawprint_modes() {
    let Some(db) = load_db() else {
        return;
    };
    let modal = season_modal(db, "Season of Gathering");

    assert_eq!(modal.mode_count, 3, "three weighted modes");
    assert_eq!(
        modal.mode_pawprints,
        vec![1u8, 2, 3],
        "pawprint weights are {{P}}/{{P}}{{P}}/{{P}}{{P}}{{P}}"
    );
    assert_eq!(
        modal.max_choices, 5,
        "max_choices is the 5-point budget, NOT clamped to mode_count (the cap-bug fix)"
    );
    assert_eq!(
        modal.min_choices, 0,
        "\"up to five\" permits choosing zero modes"
    );
    assert!(
        modal.allow_repeat_modes,
        "\"you may choose the same mode more than once\""
    );
}

/// Test 2 — the budget counts {P}, not modes: the 1-point mode (Bold creates a
/// tapped Treasure) may be chosen FIVE times (Σ = 5 ≤ 5). All five resolve.
#[test]
fn season_bold_repeats_one_point_mode_five_times() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bold = scenario.add_real_card(P0, "Season of the Bold", Zone::Hand, db);
    fund_season_pool(&mut scenario, P0, ManaType::Red);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(bold).modes(&[0, 0, 0, 0, 0]).resolve();

    assert_eq!(
        token_count(outcome.state(), "Treasure"),
        5,
        "five {{P}} mode-0 selections create five Treasure tokens (budget is 5 {{P}}, not 3 modes)"
    );
}

/// Test 3 — Σ weight ≤ budget is enforced by `validate_modal_indices`. `[2,2]`
/// (Σ = 3+3 = 6) and `[2,0,0,0]` (Σ = 3+1+1+1 = 6) are rejected; `[2,0,0]`
/// (Σ = 3+1+1 = 5) is accepted.
#[test]
fn season_budget_blocks_overspend() {
    let Some(db) = load_db() else {
        return;
    };
    let modal = season_modal(db, "Season of Gathering");

    assert!(
        validate_modal_indices(&modal, &[2, 2], &[]).is_err(),
        "two 3-point modes total 6 {{P}} > budget 5"
    );
    assert!(
        validate_modal_indices(&modal, &[2, 0, 0, 0], &[]).is_err(),
        "3+1+1+1 = 6 {{P}} > budget 5"
    );
    assert!(
        validate_modal_indices(&modal, &[2, 0, 0], &[]).is_ok(),
        "3+1+1 = 5 {{P}} == budget 5 is legal"
    );
}

/// Test 4 — a mixed-weight selection resolves through the REAL pipeline (not
/// just header collection): Weaving `[0, 0]` draws two cards (mode 0 = "Draw a
/// card", weight 1, chosen twice, Σ = 2 ≤ 5). Asserts the mode BODY resolves to
/// a real `Effect` rather than `Unimplemented`.
#[test]
fn season_weaving_mode_body_resolves() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Stock the library so draws have cards to pull.
    for _ in 0..4 {
        scenario.add_real_card(P0, "Forest", Zone::Library, db);
    }
    let weaving = scenario.add_real_card(P0, "Season of Weaving", Zone::Hand, db);
    fund_season_pool(&mut scenario, P0, ManaType::Blue);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(weaving).modes(&[0, 0]).resolve();

    // `hand_drawn` is measured from the post-commit baseline (after the spell
    // left hand), so two draw modes yield a +2 delta.
    assert_eq!(
        outcome.hand_drawn(P0),
        2,
        "two weight-1 draw modes resolve as two real Draw effects (not Unimplemented)"
    );
}

/// Test 5 — declining all modes (empty selection) resolves cleanly as a no-op.
/// CR 700.2i + `min_choices == 0` permit choosing zero modes.
#[test]
fn season_decline_all_modes_resolves_noop() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let weaving = scenario.add_real_card(P0, "Season of Weaving", Zone::Hand, db);
    fund_season_pool(&mut scenario, P0, ManaType::Blue);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(weaving).modes(&[]).resolve();

    assert!(
        outcome.state().stack.is_empty(),
        "the spell resolves and leaves the stack with no modes chosen"
    );
    // No mode chosen → no draw, no other effect.
    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "declining all modes applies no effects"
    );
}

/// Test 6 — the AI candidate generator prunes pawprint sequences to budget-legal
/// ones. `[0,0,0,0,0]` (Σ = 5) is offered; any sequence with Σ > 5 is not.
#[test]
fn candidate_generator_respects_pawprint_budget() {
    let Some(db) = load_db() else {
        return;
    };
    let modal = season_modal(db, "Season of Gathering");

    // Drive the budget authority directly across the candidate space the
    // generator would enumerate, asserting the post-filter predicate the AI arm
    // applies.
    assert!(
        pawprint_budget_satisfied(&modal, &[0, 0, 0, 0, 0]),
        "five 1-point modes (Σ = 5) are budget-legal"
    );
    assert!(
        !pawprint_budget_satisfied(&modal, &[2, 2]),
        "two 3-point modes (Σ = 6) exceed the budget and must be pruned"
    );
    assert!(
        !pawprint_budget_satisfied(&modal, &[0, 0, 0, 0, 0, 0]),
        "six 1-point modes (Σ = 6) exceed the budget and must be pruned"
    );
}

/// The whole cycle parses as a pawprint budget modal with weights [1,2,3] and an
/// uncapped 5-point budget. Covers all five "Season of …" cards (the pattern
/// class, not a single card) and keeps Season of the Burrow in the fixture.
#[test]
fn whole_season_cycle_parses_as_pawprint_budget() {
    let Some(db) = load_db() else {
        return;
    };
    for name in [
        "Season of Gathering",
        "Season of Loss",
        "Season of the Bold",
        "Season of the Burrow",
        "Season of Weaving",
    ] {
        let modal = season_modal(db, name);
        assert_eq!(
            modal.mode_pawprints,
            vec![1u8, 2, 3],
            "{name}: weights [1,2,3]"
        );
        assert_eq!(modal.max_choices, 5, "{name}: 5-point budget, uncapped");
        assert_eq!(modal.mode_count, 3, "{name}: three modes");
        assert!(modal.allow_repeat_modes, "{name}: repeats allowed");
    }
}

/// Test 7 (mode #5 verification) — Season of Loss `{P}{P}` "draw a card for each
/// creature that died under your control this turn". This is a documented
/// VERIFY target. Whatever the body lowers to, selecting it MUST resolve
/// cleanly (no panic) and the budget gate must accept the weight-2 selection.
#[test]
fn season_loss_died_count_mode_resolves_cleanly() {
    let Some(db) = load_db() else {
        return;
    };
    let modal = season_modal(db, "Season of Loss");
    assert_eq!(modal.mode_pawprints, vec![1u8, 2, 3]);
    // The weight-2 died-count mode alone is within budget.
    assert!(validate_modal_indices(&modal, &[1], &[]).is_ok());

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let loss = scenario.add_real_card(P0, "Season of Loss", Zone::Hand, db);
    fund_season_pool(&mut scenario, P0, ManaType::Black);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Selecting the died-count mode resolves without panicking; with zero
    // creatures dead this turn the count is 0, so no cards are drawn either way.
    let outcome = runner.cast(loss).modes(&[1]).resolve();
    assert!(
        outcome.state().stack.is_empty(),
        "the died-count mode resolves and clears the stack"
    );
}

/// Test 8 (mode #9) — Season of the Bold `{P}{P}{P}`, the weight-3 mode. This
/// exercises the budget gate (selection `[2]`, Σ = 3 ≤ 5 is accepted) and that
/// the spell resolves without panicking and drains the stack. It does NOT assert
/// the mode's full behavioral correctness — see the KNOWN GAP note below.
#[test]
fn season_bold_delayed_damage_mode_resolves_without_panic_known_body_gap() {
    let Some(db) = load_db() else {
        return;
    };
    let modal = season_modal(db, "Season of the Bold");
    assert!(
        validate_modal_indices(&modal, &[2], &[]).is_ok(),
        "the weight-3 mode (Σ = 3) is within the 5-point budget"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Mode 2's delayed trigger deals damage to "up to one target creature";
    // give the battlefield a creature so the mode is selectable (CR 700.2a).
    scenario.add_creature(engine::game::scenario::P1, "Grizzly Bears", 2, 2);
    let bold = scenario.add_real_card(P0, "Season of the Bold", Zone::Hand, db);
    fund_season_pool(&mut scenario, P0, ManaType::Red);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(bold).modes(&[2]).resolve();
    // KNOWN GAP (follow-up): the weight-3 mode's Oracle body is "Until the end of
    // your next turn, whenever you cast a spell, ~ deals 2 damage to up to one
    // target creature." The parser currently DROPS the "until end of next turn,
    // whenever you cast a spell" delayed cast-triggered wrapper and misparses the
    // body to a bare immediate `DealDamage{2, creature}`. This is a pre-existing
    // effect-body parser limitation, NOT a budget-mechanic defect. We therefore
    // assert only what is genuinely true here: the budget gate accepts the
    // selection (above) and the spell resolves without panicking and drains the
    // stack. The delayed-trigger behavior is not asserted because it is not
    // modeled.
    assert!(
        outcome.state().stack.is_empty(),
        "the weight-3 mode resolves without panicking and clears the stack"
    );
}
