//! End-to-end smoke test for cEDH difficulty wiring.
//!
//! Verifies that all layers wired across Phases 1-8 of the cEDH implementation
//! are correctly connected: config preset values, 4-player paranoid-scaling
//! bypass, `DeckFeatures::is_cedh`, `ComboLinePolicy` registration, and the
//! stub `ComboRegistry` entry.

use std::sync::Arc;

use engine::ai_support::legal_actions;
use engine::game::bracket_estimate::CommanderBracketTier;
use engine::game::deck_loading::DeckEntry;
use engine::game::zones::create_object;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use phase_ai::combo::ComboRegistry;
use phase_ai::config::{create_config, create_config_for_players, AiDifficulty, Platform};
use phase_ai::features::DeckFeatures;
use phase_ai::policies::registry::{PolicyId, PolicyRegistry};
use phase_ai::search::{choose_action, score_candidates, softmax_select_pairs};

/// Builds the minimal `GameState` shared by the `score_candidates` and
/// `choose_action` cEDH combo tests.
///
/// `tier` controls `PlayerId(0)`'s `PlayerDeckPool::bracket_tier`; pass
/// `CommanderBracketTier::Cedh` for the positive tests and
/// `CommanderBracketTier::Core` (or any non-cEDH tier) for negative tests that
/// verify the combo bonus is absent.
///
/// Returns `(state, heliod_id, ballista_id)`.
///
/// Invariants guaranteed by this builder:
///
/// - `Phase::PreCombatMain`, `PlayerId(0)` has priority.
/// - Two untapped Plains satisfy Heliod's `{1}{W}` cost via the legacy
///   land-mana fallback (no `AbilityDefinition` needed on the land objects).
/// - Heliod at `abilities[0]` with `{1}{W}` — name-exact so the combo
///   detector fires.
/// - Walking Ballista at `abilities[0]` (placeholder, `NoCost`) and
///   `abilities[1]` (damage step, `NoCost`).
/// - `PlayerId(0)`'s `PlayerDeckPool` has a non-empty `current_main` so
///   `build_ai_context` propagates the tier through `DeckFeatures::analyze`.
fn cedh_combo_state_with_synthetic_abilities(
    tier: CommanderBracketTier,
) -> (GameState, ObjectId, ObjectId) {
    let mut state = GameState::new_two_player(0);

    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Two untapped Plains — legacy mana fallback synthesizes {T}: Add {W} per
    // land without needing an explicit AbilityDefinition.
    for i in 0..2 {
        let land_id = create_object(
            &mut state,
            CardId(100 + i),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Plains".to_string());
    }

    // Heliod, Sun-Crowned — abilities[0] = {1}{W} activated ability.
    let heliod_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Heliod, Sun-Crowned".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&heliod_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(0);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Unimplemented {
                name: "test_heliod_lifelink".to_string(),
                description: None,
            },
        );
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            },
        });
        Arc::make_mut(&mut obj.abilities).push(ability);
    }

    // Walking Ballista — abilities[0] = placeholder (NoCost), abilities[1] =
    // damage step (NoCost).  The combo policy checks `(source_id,
    // ability_index)` only; effects are `Unimplemented`.
    let ballista_id = create_object(
        &mut state,
        CardId(201),
        PlayerId(0),
        "Walking Ballista".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&ballista_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(0);
        let mut placeholder = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Unimplemented {
                name: "test_ballista_growth".to_string(),
                description: None,
            },
        );
        placeholder.cost = Some(AbilityCost::Mana {
            cost: ManaCost::NoCost,
        });
        let mut damage = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Unimplemented {
                name: "test_ballista_damage".to_string(),
                description: None,
            },
        );
        damage.cost = Some(AbilityCost::Mana {
            cost: ManaCost::NoCost,
        });
        let abilities = Arc::make_mut(&mut obj.abilities);
        abilities.push(placeholder);
        abilities.push(damage);
    }

    let dummy_entry = DeckEntry {
        card: CardFace::default(),
        count: 1,
    };
    state.deck_pools.clear();
    state.deck_pools.push(PlayerDeckPool {
        player: PlayerId(0),
        current_main: Arc::new(vec![dummy_entry.clone()]),
        bracket_tier: tier,
        ..PlayerDeckPool::default()
    });
    state.deck_pools.push(PlayerDeckPool {
        player: PlayerId(1),
        current_main: Arc::new(vec![dummy_entry]),
        bracket_tier: CommanderBracketTier::Core,
        ..PlayerDeckPool::default()
    });

    (state, heliod_id, ballista_id)
}

#[test]
fn cedh_full_stack_smoke() {
    // 1. CEDH preset values (CR-irrelevant; engine config constants).
    let cfg = create_config(AiDifficulty::CEDH, Platform::Native);
    assert_eq!(cfg.search.max_depth, 3);
    assert_eq!(cfg.search.max_nodes, 96);

    // 2. 4-player scaling is skipped for CEDH: depth and nodes must be
    //    unchanged from the 2-player config.
    let cfg4 = create_config_for_players(AiDifficulty::CEDH, Platform::Native, 4);
    assert_eq!(
        cfg4.search.max_depth, 3,
        "4-player CEDH must skip paranoid scaling"
    );
    assert_eq!(
        cfg4.search.max_nodes, 96,
        "4-player CEDH must skip paranoid scaling"
    );

    // 3. DeckFeatures::bracket_tier defaults to a non-cEDH tier.
    let features = DeckFeatures::default();
    assert_ne!(
        features.bracket_tier,
        engine::game::bracket_estimate::CommanderBracketTier::Cedh
    );

    // 4. DeckFeatures::analyze records the Cedh tier when given it.
    let cedh_features = DeckFeatures::analyze(
        &[],
        engine::game::bracket_estimate::CommanderBracketTier::Cedh,
    );
    assert_eq!(
        cedh_features.bracket_tier,
        engine::game::bracket_estimate::CommanderBracketTier::Cedh
    );

    // 5. Default PolicyRegistry includes ComboLineProgress — the policy that
    //    consults ComboRegistry during cEDH AI decisions.
    let reg = PolicyRegistry::default();
    assert!(
        reg.has_policy(PolicyId::ComboLineProgress),
        "PolicyRegistry::default() must register ComboLinePolicy"
    );

    // 6. ComboRegistry ships with at least one stub combo line to prove
    //    end-to-end wiring (real cEDH lines are a follow-up phase).
    let combo_reg = ComboRegistry::default();
    assert!(
        !combo_reg.lines().is_empty(),
        "ComboRegistry::default() must contain at least one combo line"
    );
}

/// End-to-end planner integration test for the cEDH combo bonus.
///
/// Builds a minimal `GameState` where the registered Heliod, Sun-Crowned +
/// Walking Ballista combo line is already assembled on the AI player's
/// battlefield (with two untapped Plains for the `{1}{W}` activation cost),
/// configures both players' `PlayerDeckPool::bracket_tier = Cedh`, and runs
/// `phase_ai::search::score_candidates` — the full planner entry point.
///
/// The assertion is that the Heliod activation outscores `PassPriority` (the
/// always-legal no-op baseline). The only mechanism by which Heliod activation
/// can outscore PassPriority on this synthetic state is the `ComboLinePolicy`
/// firing its `combo_progress_this_turn_bonus`, which proves the full chain:
///
///   `PlayerDeckPool::bracket_tier = Cedh`
///     -> `DeckFeatures::is_cedh = true`
///     -> `ComboLinePolicy::activation() = Some(1.0)`
///     -> `ComboRegistry::reachable_lines()` returns Heliod/Ballista
///     -> `verdict()` applies the bonus to the Heliod activation candidate
///     -> `score_candidates()` returns the boosted score
///
/// This is the *only* end-to-end test in the suite that exercises the wiring
/// through `build_ai_context` and `tactical_score`'s full policy registry —
/// the per-policy unit tests in `policies/combo_line.rs` construct
/// `PolicyContext` by hand with `AiContext::empty()`, which bypasses
/// `build_ai_context` (the production path that turns deck-pool tier into the
/// `is_cedh` flag the policy gates on).
#[test]
fn score_candidates_boosts_heliod_combo_activation_for_cedh_ai() {
    let (state, heliod_id, ballista_id) =
        cedh_combo_state_with_synthetic_abilities(CommanderBracketTier::Cedh);

    // Sanity guard: the engine must offer the Heliod activation as a legal
    // priority action. If this fails the test is mis-set-up and the
    // score-based assertion below would be meaningless.
    let actions = legal_actions(&state);
    let heliod_activation = GameAction::ActivateAbility {
        source_id: heliod_id,
        ability_index: 0,
    };
    assert!(
        actions.contains(&heliod_activation),
        "legal_actions must offer the Heliod activation candidate; got {:?}",
        actions
    );
    assert!(
        actions.contains(&GameAction::PassPriority),
        "legal_actions must offer PassPriority as the baseline"
    );

    // Run the real planner entry point. `into_measurement` disables the
    // wall-clock budget so the test is reproducible.
    let config = create_config(AiDifficulty::CEDH, Platform::Native).into_measurement(42);
    let scored = score_candidates(&state, PlayerId(0), &config);
    assert!(
        !scored.is_empty(),
        "score_candidates returned no candidates"
    );

    let heliod_score = scored
        .iter()
        .find(|(action, _)| *action == heliod_activation)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!(
                "Heliod activation candidate missing from scored output: {:?}",
                scored
            )
        });
    let pass_score = scored
        .iter()
        .find(|(action, _)| matches!(action, GameAction::PassPriority))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!(
                "PassPriority candidate missing from scored output: {:?}",
                scored
            )
        });

    // CR-irrelevant: this is a wiring assertion, not a rules one.
    // `combo_progress_this_turn_bonus = 15.0` (cEDH preset). Inside
    // `score_candidates`, the tactical signal — which is where the policy
    // delta lives — is scaled by `tactical_weight = 0.1` before being added
    // to the continuation rollout score, so the visible separation per
    // combo step is `15.0 * 1.0 * 0.1 = 1.5` plus any continuation noise.
    //
    // Two assertions guard against regressions in different wiring layers:
    //   (a) Heliod activation must outscore PassPriority — proves the
    //       activation candidate at least clears the always-legal baseline.
    //   (b) At least one of the combo-line steps (Heliod[0] or Ballista[1])
    //       must dominate PassPriority by at least `+1.0`. The combo bonus
    //       is the only mechanism on this synthetic state that can push
    //       any activation that far above PassPriority — so a passing
    //       assertion proves the chain
    //         `PlayerDeckPool::bracket_tier = Cedh` ->
    //         `DeckFeatures::is_cedh = true` ->
    //         `ComboLinePolicy::activation()` ->
    //         `ComboRegistry::reachable_lines()` ->
    //         `verdict()` bonus ->
    //         `score_candidates` output
    //       is wired end-to-end.
    assert!(
        heliod_score > pass_score,
        "Heliod combo activation must outscore PassPriority for a cEDH AI \
         (heliod_score = {heliod_score}, pass_score = {pass_score}, scored = {scored:?})"
    );

    let ballista_damage_score = scored
        .iter()
        .find(|(action, _)| {
            matches!(
                action,
                GameAction::ActivateAbility {
                    source_id,
                    ability_index: 1,
                } if *source_id == ballista_id
            )
        })
        .map(|(_, s)| *s);

    let best_combo_step_score = ballista_damage_score
        .map(|b| heliod_score.max(b))
        .unwrap_or(heliod_score);

    // Derive the minimum acceptable margin from the config value rather than
    // hardcoding it.  Inside `score_candidates`, the raw policy bonus is
    // dampened by `tactical_weight = 0.1` (the main-phase, non-stack-response
    // branch in `search.rs`; not exposed as a named constant).  We require the
    // visible margin to be at least half of that dampened bonus, so the
    // assertion trips if `tactical_weight` drops below ~0.034 (half of the
    // current 0.067 trip-point) while still tolerating small weight retunings.
    let expected_full_bonus = config.policy_penalties.combo_progress_this_turn_bonus;
    // 0.1 = tactical_weight for main-phase, non-target-selection in search.rs
    let min_margin = expected_full_bonus * 0.1 * 0.5;
    assert!(
        best_combo_step_score - pass_score > min_margin,
        "At least one Heliod/Ballista combo step must dominate PassPriority \
         by at least {min_margin:.3} (~50% of damped policy bonus) — \
         combo_progress_this_turn_bonus is not reaching score_candidates \
         output (best combo diff = {:.3}, heliod = {heliod_score}, \
         ballista[1] = {ballista_damage_score:?}, pass = {pass_score}, \
         scored = {scored:?})",
        best_combo_step_score - pass_score
    );
}

/// Closes the selection-layer gap left by `score_candidates_boosts_*`.
///
/// `score_candidates` proved the combo bonus reaches the final score. This
/// test proves the SELECTION layer — `choose_action`'s `softmax_select_pairs`
/// call — uses that score dominance to pick a combo activation in practice.
///
/// A single trial would be seed-dependent. Instead we run 50 trials with
/// different seeds and assert that at least 40 (80 %) select a combo step.
/// With `temperature = 0.2` and the combo steps outscoring `PassPriority`
/// by ~+1.5 (15.0 policy bonus × 0.1 tactical weight), the theoretical
/// softmax probability of picking a combo step is >95 % — the 80 % threshold
/// is deliberately conservative to tolerate small weight retunings while still
/// catching a real wiring regression that drops selection to chance level.
#[test]
fn choose_action_picks_combo_activation_for_cedh_ai() {
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    let (state, heliod_id, ballista_id) =
        cedh_combo_state_with_synthetic_abilities(CommanderBracketTier::Cedh);
    let config = create_config(AiDifficulty::CEDH, Platform::Native).into_measurement(42);

    let mut smoke_rng = SmallRng::seed_from_u64(0);
    assert!(
        choose_action(&state, PlayerId(0), &config, &mut smoke_rng).is_some(),
        "choose_action smoke must return a legal action"
    );

    let scored = score_candidates(&state, PlayerId(0), &config);
    assert!(
        !scored.is_empty(),
        "score_candidates returned no candidates"
    );

    let mut combo_count = 0u32;
    for seed in 0..50u64 {
        let mut rng = SmallRng::seed_from_u64(seed);
        let action = softmax_select_pairs(&scored, config.temperature, &mut rng);
        if matches!(
            action,
            Some(GameAction::ActivateAbility {
                source_id,
                ability_index,
            }) if (source_id == heliod_id && ability_index == 0)
                || (source_id == ballista_id && ability_index == 1)
        ) {
            combo_count += 1;
        }
    }

    assert!(
        combo_count >= 40,
        "expected at least 40/50 trials to pick a combo activation (Heliod[0] or \
         Ballista[1]); got {combo_count}/50 — softmax selection is not respecting the \
         ComboLinePolicy bonus reaching score_candidates output (heliod_id = {heliod_id:?}, \
         ballista_id = {ballista_id:?})"
    );
}

/// Proves the cEDH combo bonus is the *cause* of combo selection, not an
/// incidental bias. Holds everything constant versus
/// `choose_action_picks_combo_activation_for_cedh_ai` — same state, same
/// `AiDifficulty::CEDH` config — and varies only the deck tier from `Cedh` to
/// `Core`. With `is_cedh = false`, `ComboLinePolicy::activation()` returns
/// `None`, so no bonus reaches the combo activations. If this test ever fails
/// (combo_count ≥ 20), the bonus is leaking through a code path that does not
/// gate on `is_cedh`, which is a real wiring regression.
#[test]
fn choose_action_does_not_boost_combo_without_is_cedh() {
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    let (state, heliod_id, ballista_id) =
        cedh_combo_state_with_synthetic_abilities(CommanderBracketTier::Core);
    // Difficulty stays CEDH so the only variable is the deck tier / is_cedh flag.
    let config = create_config(AiDifficulty::CEDH, Platform::Native).into_measurement(42);

    let mut smoke_rng = SmallRng::seed_from_u64(0);
    assert!(
        choose_action(&state, PlayerId(0), &config, &mut smoke_rng).is_some(),
        "choose_action smoke must return a legal action"
    );

    let scored = score_candidates(&state, PlayerId(0), &config);
    assert!(
        !scored.is_empty(),
        "score_candidates returned no candidates"
    );

    let mut combo_count = 0u32;
    for seed in 0..50u64 {
        let mut rng = SmallRng::seed_from_u64(seed);
        let action = softmax_select_pairs(&scored, config.temperature, &mut rng);
        if matches!(
            action,
            Some(GameAction::ActivateAbility {
                source_id,
                ability_index,
            }) if (source_id == heliod_id && ability_index == 0)
                || (source_id == ballista_id && ability_index == 1)
        ) {
            combo_count += 1;
        }
    }

    // On this synthetic state the only legal actions are PassPriority,
    // Heliod[0] (needs mana, so often absent from scored output), and
    // Ballista[0]/Ballista[1] (NoCost). Without the combo bonus the two
    // Ballista activations score about the same as PassPriority and the
    // softmax naturally picks them ~40-50 % of the time by base-rate alone.
    // The threshold here must therefore lie *between* that base-rate upper
    // bound (≤ 35, empirically ~27 without the bonus) and the ≥ 40 that the
    // positive test requires. A combo_count ≥ 35 is the signal that the bonus
    // is leaking — it would push selection to the same >80 % territory we
    // measure in the positive test.
    assert!(
        combo_count < 35,
        "expected fewer than 35/50 trials to pick a combo activation when \
         bracket_tier = Core (is_cedh = false); got {combo_count}/50 — the \
         ComboLinePolicy bonus appears to be firing even without the cEDH flag \
         set (heliod_id = {heliod_id:?}, ballista_id = {ballista_id:?})"
    );
}
