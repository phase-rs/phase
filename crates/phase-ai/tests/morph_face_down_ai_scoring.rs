//! Slice E (branch 2a) — the AI sanely ENUMERATES and SCORES the face-down cast.
//!
//! The engine PR "morph/disguise face-down spell casting" adds a new legal action:
//! casting a Morph/Megamorph/Disguise creature face down for {3} via
//! `WaitingFor::AlternativeCastChoice { keyword: FaceDown }`. These tests guard the
//! AI decision layer (non-vacuity condition for the AI half of the PR):
//!
//! - (i) the face-down cast is a REAL, enumerated candidate — produced by
//!   `engine::ai_support::candidate_actions` (keyword-agnostic `AlternativeCastChoice`
//!   arm) and consumed by the phase-ai search via `build_decision_context`
//!   (`choose_action` -> `score_candidates` -> `build_decision_context`). Not a
//!   decline-by-omission.
//! - (ii) the eval CREDITS the resulting face-down permanent AS A 2/2: for a strong
//!   real creature that is castable, the normal cast (a 5/5) must OUTSCORE the
//!   face-down cast (a 2/2). Identical scores would mean the eval is blind to the
//!   blanking (e.g. wrongly crediting the hidden 5/5); a sentinel would mean the
//!   candidate was rejected, not evaluated.
//!
//! Emergent (NOT pinned — eval-weight-sensitive, deliberately untested): in a
//! hand-check the AI *does* prefer the face-down cast when it is the better body
//! (a 1/1 real creature vs a 2/2 face-down for less mana), so the eval can value
//! face-down UP as well as down. We do not assert that ordering — a small eval
//! weight shift could flip a 1-power gap, and a flaky preference test is worse than
//! an honest "enumeration + sane 2/2 ordering are tested; active preference is
//! emergent." The engine offer + restricted-mana payment are covered by
//! `engine_tests::{morph_creature_offers_face_down_alternative_cast,
//! tin_street_gossip_restricted_mana_funds_face_down_cast}`.

use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{AlternativeCastKeyword, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use phase_ai::config::{create_config, AiDifficulty, Platform};

const MORPH_ORACLE: &str = "Morph {4} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)";

/// Build P0 with a `power`/`toughness` morph creature (printed cost {5}, Morph {4})
/// in hand plus enough colorless mana for BOTH the printed {5} and the face-down
/// {3}, then cast it to reach the face-down alternative-cast choice.
fn morph_at_alternative_choice(power: i32, toughness: i32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Morpher", power, toughness, MORPH_ORACLE)
        .with_mana_cost(ManaCost::generic(5))
        .from_oracle_text_with_keywords(&["Morph"], MORPH_ORACLE)
        .id();
    let mut runner = scenario.build();

    // Reach-guard: the object must actually carry Morph, else the offer never
    // fires and the assertions would pass vacuously.
    assert!(
        runner.state().objects[&morph]
            .base_keywords
            .iter()
            .any(|k| matches!(k, engine::types::keywords::Keyword::Morph(_))),
        "precondition: the creature must carry Keyword::Morph"
    );

    for _ in 0..5 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    let card_id = runner.state().objects[&morph].card_id;
    apply_as_current(
        runner.state_mut(),
        GameAction::CastSpell {
            object_id: morph,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("casting the morph creature must be accepted");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::FaceDown,
                ..
            }
        ),
        "precondition: must be at AlternativeCastChoice(FaceDown), got {:?}",
        runner.state().waiting_for
    );
    (runner, morph)
}

fn score_of(scored: &[(GameAction, f64)], choice: AlternativeCastDecision) -> Option<f64> {
    scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::ChooseAlternativeCast { choice: c } if *c == choice))
        .map(|(_, s)| *s)
}

/// (i) The AI candidate set for a morph-creature `AlternativeCastChoice` state
/// CONTAINS both the face-down (Alternative) and the printed (Normal) cast — the
/// face-down cast is genuinely enumerated, not declined by omission.
#[test]
fn ai_enumerates_both_face_down_and_normal_cast() {
    let (runner, _morph) = morph_at_alternative_choice(5, 5);
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let scored = phase_ai::search::score_candidates(runner.state(), P0, &config);

    assert!(
        score_of(&scored, AlternativeCastDecision::Alternative).is_some(),
        "the face-down (Alternative) cast must be an enumerated candidate"
    );
    assert!(
        score_of(&scored, AlternativeCastDecision::Normal).is_some(),
        "the Normal cast must be an enumerated candidate"
    );
}

/// (ii, strengthened) The eval CREDITS the face-down permanent AS A 2/2: for a
/// strong real creature that is castable, the normal cast (a 5/5) must OUTSCORE
/// the face-down cast (a 2/2). Distinct + correctly ordered scores prove the eval
/// distinguishes the two boards (not blind to the blanking, not sentinel-rejected).
#[test]
fn ai_scores_face_down_2_2_below_a_strong_real_creature() {
    let (runner, _morph) = morph_at_alternative_choice(5, 5);
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let scored = phase_ai::search::score_candidates(runner.state(), P0, &config);

    let alt = score_of(&scored, AlternativeCastDecision::Alternative)
        .expect("face-down cast must be enumerated + scored");
    let normal = score_of(&scored, AlternativeCastDecision::Normal)
        .expect("normal cast must be enumerated + scored");

    // Evaluated, not sentinel-rejected.
    assert!(
        alt.is_finite() && normal.is_finite(),
        "both cast options must receive finite (evaluated) scores: alt={alt}, normal={normal}"
    );
    assert!(
        alt.abs() < 1.0e6 && normal.abs() < 1.0e6,
        "neither score may be a reject/veto sentinel: alt={alt}, normal={normal}"
    );
    // The discriminating assertion: a 5/5 real body must outscore a 2/2 face-down
    // body (same mana available). Equal scores would mean the eval failed to credit
    // the face-down permanent as the blank 2/2 it actually is (CR 708.2).
    assert!(
        normal > alt,
        "casting a 5/5 normally must outscore casting it face down as a 2/2 \
         (normal={normal}, face_down={alt}) — the eval must value the 2/2 AS a 2/2"
    );
}
