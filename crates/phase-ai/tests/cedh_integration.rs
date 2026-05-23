//! End-to-end smoke test for cEDH difficulty wiring.
//!
//! Verifies that all layers wired across Phases 1-8 of the cEDH implementation
//! are correctly connected: config preset values, 4-player paranoid-scaling
//! bypass, `DeckFeatures::is_cedh`, `ComboLinePolicy` registration, and the
//! stub `ComboRegistry` entry.

use phase_ai::combo::ComboRegistry;
use phase_ai::config::{create_config, create_config_for_players, AiDifficulty, Platform};
use phase_ai::features::DeckFeatures;
use phase_ai::policies::registry::{PolicyId, PolicyRegistry};

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

    // 3. DeckFeatures::is_cedh is false by default (non-cEDH bracket).
    let features = DeckFeatures::default();
    assert!(!features.is_cedh);

    // 4. DeckFeatures::analyze sets is_cedh = true when the bracket tier is
    //    CommanderBracketTier::Cedh.
    let cedh_features = DeckFeatures::analyze(
        &[],
        engine::game::bracket_estimate::CommanderBracketTier::Cedh,
    );
    assert!(cedh_features.is_cedh);

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
