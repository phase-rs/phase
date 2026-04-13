//! Static plan derivation — turns a `DeckFeatures` prior into a
//! `PlanSnapshot` describing the expected curve of lands, mana, and threats
//! across the first 15 turns.
//!
//! Consumed once per game by `AiSession::from_game`. Live realization against
//! the current board lives in `plan/mod.rs::PlanState` (not exercised by
//! Phase B).

use crate::deck_profile::DeckArchetype;
use crate::features::tribal::{
    AGGRO_TEMPO_FLOOR as TRIBAL_AGGRO_TEMPO_FLOOR, MULLIGAN_FLOOR as TRIBAL_MULLIGAN_FLOOR,
};
use crate::features::DeckFeatures;

use super::{PlanSnapshot, TempoClass};

const SCHEDULE_LEN: usize = 15;

/// Derive a `PlanSnapshot` from deck features. The snapshot models a deck's
/// expected curve — it does not depend on game state and is cached per game.
pub fn derive_snapshot(features: &DeckFeatures) -> PlanSnapshot {
    let tempo_class = tempo_class_for(features);
    let expected_lands = expected_lands_for(features);
    let expected_mana = expected_mana_for(features);
    let expected_threats = expected_threats_for(features);

    PlanSnapshot {
        expected_lands,
        expected_mana,
        expected_threats,
        tempo_class,
    }
}

fn tempo_class_for(features: &DeckFeatures) -> TempoClass {
    // Landfall and mana_ramp commitment both bias toward Ramp regardless of
    // coarse archetype — both play like ramp decks in practice (extra lands
    // per turn, threats scale with resources).
    if features.landfall.commitment > 0.5 || features.mana_ramp.commitment > 0.5 {
        return TempoClass::Ramp;
    }
    // A tribal deck with high commitment plays aggressively — the lord-anthem
    // pattern means threat density and attack pressure dominate the game plan.
    // Placed AFTER the ramp branch so a tribal+ramp hybrid reads as Ramp.
    if features.tribal.commitment > TRIBAL_AGGRO_TEMPO_FLOOR {
        return TempoClass::Aggro;
    }
    // A control deck with high commitment AND meaningful reactive_tempo reads
    // as Control. Placed after ramp and tribal so ramp+control hybrids read as
    // Ramp and tribal+control hybrids read as Aggro (both have higher priority).
    if features.control.commitment > 0.55 && features.control.reactive_tempo > 0.35 {
        return TempoClass::Control;
    }
    // Aristocrats decks play grindy midrange — they win by attrition through
    // sacrifice loops, not burst tempo or curve aggression.
    if features.aristocrats.commitment > 0.5 {
        return TempoClass::Midrange;
    }
    match features.archetype {
        DeckArchetype::Aggro => TempoClass::Aggro,
        DeckArchetype::Control => TempoClass::Control,
        DeckArchetype::Combo => TempoClass::Combo,
        DeckArchetype::Ramp => TempoClass::Ramp,
        DeckArchetype::Midrange => TempoClass::Midrange,
    }
}

fn expected_lands_for(features: &DeckFeatures) -> [u8; SCHEDULE_LEN] {
    let mut lands = [0u8; SCHEDULE_LEN];
    // Baseline: one land drop per turn, capped at turn 5 curve.
    for (turn_idx, slot) in lands.iter_mut().enumerate() {
        let turn = (turn_idx + 1) as u8;
        *slot = turn.min(6);
    }
    // A single `wants_ramp_curve` gate prevents double-bumping when both
    // landfall and mana_ramp are high — both features indicate the same
    // "play more lands" intention, so one +1 application is correct.
    // CR 305.2: additional land drops from Azusa-likes raise the per-turn cap.
    let wants_ramp_curve =
        features.landfall.commitment > 0.5 || features.mana_ramp.commitment > 0.3;
    if wants_ramp_curve {
        for (turn_idx, slot) in lands.iter_mut().enumerate().skip(2) {
            if turn_idx < 4 {
                *slot = slot.saturating_add(1);
            } else {
                // Preserve the forward curve — everything from turn 5 onward
                // inherits the same +1 until the cap.
                *slot = slot.saturating_add(1).min(8);
            }
        }
    }
    lands
}

/// Expected available mana per turn — starts from land projections and adds
/// the contribution of dorks / rituals that can be played on earlier turns.
///
/// CR 106.4: mana pools empty each step; `expected_mana` models per-turn
/// availability, not accumulated totals. CR 305.2: additional land drops
/// further raise the ceiling when `wants_ramp_curve`.
pub(crate) fn expected_mana_for(features: &DeckFeatures) -> [u8; SCHEDULE_LEN] {
    let mut mana = expected_lands_for(features);
    // When significant mana ramp is present, model one extra mana on turns 2
    // and 3 (a dork played on turn 1 starts contributing on turn 2) and two
    // extra mana on turns 4–6 (compounded ramp: dorks + fetch lands).
    if features.mana_ramp.commitment > 0.3 {
        for (turn_idx, slot) in mana.iter_mut().enumerate() {
            let bonus: u8 = match turn_idx + 1 {
                2 | 3 => 1,
                4..=6 => 2,
                _ => 0,
            };
            *slot = slot.saturating_add(bonus).min(10);
        }
    }
    mana
}

fn expected_threats_for(features: &DeckFeatures) -> [u8; SCHEDULE_LEN] {
    let mut threats = [0u8; SCHEDULE_LEN];
    // Conservative default — one threat per two turns after turn 2. Aggro
    // front-loads, control delays.
    for (turn_idx, slot) in threats.iter_mut().enumerate() {
        let turn = (turn_idx + 1) as u8;
        *slot = match features.archetype {
            DeckArchetype::Aggro => turn.saturating_sub(1).min(5),
            DeckArchetype::Control => turn.saturating_sub(3).min(4),
            _ => turn.saturating_sub(2).min(5),
        };
    }
    // Tribal decks with meaningful commitment front-load creature deployment
    // on turns 2–4 — each lord or tribe member now matters, so threat density
    // peaks early to maximize lord anthem value.
    if features.tribal.commitment > TRIBAL_MULLIGAN_FLOOR {
        for (turn_idx, slot) in threats.iter_mut().enumerate() {
            let turn = turn_idx + 1;
            if (2..=4).contains(&turn) {
                *slot = slot.saturating_add(1);
            }
        }
    }
    // Control decks with high commitment delay threats — they spend early turns
    // deploying interaction, not permanents. Subtract 1 from turns 3 and 4
    // to reflect that control's threat deployment is back-loaded.
    if features.control.commitment > 0.55 {
        for (turn_idx, slot) in threats.iter_mut().enumerate() {
            let turn = turn_idx + 1;
            if turn == 3 || turn == 4 {
                *slot = slot.saturating_sub(1);
            }
        }
    }
    threats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::tribal::TribalFeature;
    use crate::features::{DeckFeatures, LandfallFeature, ManaRampFeature};

    #[test]
    fn landfall_commitment_bumps_turn_three_and_four_lands() {
        let mut features = DeckFeatures::default();
        let baseline = derive_snapshot(&features);

        features.landfall = LandfallFeature {
            commitment: 0.9,
            payoff_count: 3,
            enabler_count: 4,
            payoff_names: vec!["Payoff".to_string()],
        };
        let bumped = derive_snapshot(&features);

        assert_eq!(bumped.expected_lands[2], baseline.expected_lands[2] + 1);
        assert_eq!(bumped.expected_lands[3], baseline.expected_lands[3] + 1);
    }

    #[test]
    fn high_landfall_commitment_picks_ramp_tempo() {
        let features = DeckFeatures {
            landfall: LandfallFeature {
                commitment: 0.9,
                ..Default::default()
            },
            ..Default::default()
        };
        let snapshot = derive_snapshot(&features);
        assert_eq!(snapshot.tempo_class, TempoClass::Ramp);
    }

    #[test]
    fn empty_features_produces_midrange_default() {
        let snapshot = derive_snapshot(&DeckFeatures::default());
        assert_eq!(snapshot.tempo_class, TempoClass::Midrange);
    }

    #[test]
    fn ramp_commitment_bumps_expected_mana_turn_two_and_three() {
        let no_ramp = DeckFeatures::default();
        let baseline_mana = expected_mana_for(&no_ramp);
        // Turn 2 baseline: 2 (lands) + 0 (no ramp bonus). Turn 3: 3.
        assert_eq!(baseline_mana[1], 2, "baseline turn 2");
        assert_eq!(baseline_mana[2], 3, "baseline turn 3");

        let features = DeckFeatures {
            mana_ramp: ManaRampFeature {
                dork_count: 4,
                commitment: 0.48, // > 0.3 triggers both land and mana bumps
                ..Default::default()
            },
            ..Default::default()
        };
        let ramped = expected_mana_for(&features);

        // Turn 2 (index 1): land baseline 2 + mana ramp +1 = 3.
        assert_eq!(
            ramped[1], 3,
            "turn 2 mana should be bumped by +1 (mana bonus)"
        );
        // Turn 3 (index 2): land baseline 3 + land ramp bump +1 + mana ramp +1 = 5.
        assert_eq!(
            ramped[2], 5,
            "turn 3 mana should be bumped by +2 (land + mana bonus)"
        );
        // Turn 4 (index 3): land baseline 4 + land ramp bump +1 + mana ramp +2 = 7.
        assert_eq!(
            ramped[3], 7,
            "turn 4 mana should be bumped by +3 (land + mana bonus)"
        );
        // Verify it's strictly higher than baseline at the important early turns.
        assert!(
            ramped[1] > baseline_mana[1],
            "turn 2 mana must exceed baseline"
        );
        assert!(
            ramped[2] > baseline_mana[2],
            "turn 3 mana must exceed baseline"
        );
    }

    #[test]
    fn ramp_and_landfall_stack_is_idempotent() {
        // Both landfall (> 0.5) and mana_ramp (> 0.3) active — the
        // `wants_ramp_curve` gate in `expected_lands_for` must apply
        // `saturating_add(1)` exactly once, not twice.
        let baseline_lands = expected_lands_for(&DeckFeatures::default());

        let both_features = DeckFeatures {
            landfall: LandfallFeature {
                commitment: 0.9,
                ..Default::default()
            },
            mana_ramp: ManaRampFeature {
                dork_count: 4,
                commitment: 0.48,
                ..Default::default()
            },
            ..Default::default()
        };
        let both_lands = expected_lands_for(&both_features);

        // Turn 3 (index 2) must be exactly +1 above baseline — not +2.
        assert_eq!(
            both_lands[2],
            baseline_lands[2] + 1,
            "double-bump guard: turn 3 land should be exactly +1"
        );
    }

    #[test]
    fn high_ramp_commitment_picks_ramp_tempo() {
        let features = DeckFeatures {
            mana_ramp: ManaRampFeature {
                dork_count: 8,
                commitment: 0.96,
                ..Default::default()
            },
            ..Default::default()
        };
        let snapshot = derive_snapshot(&features);
        assert_eq!(snapshot.tempo_class, TempoClass::Ramp);
    }

    #[test]
    fn tribal_commitment_bumps_threats() {
        let baseline = derive_snapshot(&DeckFeatures::default());

        let features = DeckFeatures {
            tribal: TribalFeature {
                dominant_tribe: Some("Elf".to_string()),
                commitment: 0.7,
                tribes: Vec::new(),
            },
            ..Default::default()
        };
        let bumped = derive_snapshot(&features);

        // Turns 2–4 (indices 1–3) should each be +1.
        assert_eq!(
            bumped.expected_threats[1],
            baseline.expected_threats[1] + 1,
            "turn 2 threats should be bumped"
        );
        assert_eq!(
            bumped.expected_threats[2],
            baseline.expected_threats[2] + 1,
            "turn 3 threats should be bumped"
        );
        assert_eq!(
            bumped.expected_threats[3],
            baseline.expected_threats[3] + 1,
            "turn 4 threats should be bumped"
        );
        // Turn 1 and 5+ should be unchanged.
        assert_eq!(
            bumped.expected_threats[0], baseline.expected_threats[0],
            "turn 1 should not be bumped"
        );
        assert_eq!(
            bumped.expected_threats[4], baseline.expected_threats[4],
            "turn 5 should not be bumped"
        );
    }

    #[test]
    fn high_tribal_commitment_picks_aggro_tempo() {
        let features = DeckFeatures {
            tribal: TribalFeature {
                dominant_tribe: Some("Goblin".to_string()),
                commitment: 0.8,
                tribes: Vec::new(),
            },
            ..Default::default()
        };
        let snapshot = derive_snapshot(&features);
        assert_eq!(snapshot.tempo_class, TempoClass::Aggro);
    }

    #[test]
    fn tribal_plus_ramp_picks_ramp_tempo() {
        // Ramp branch fires before tribal — tribal+ramp hybrid reads as Ramp.
        let features = DeckFeatures {
            tribal: TribalFeature {
                dominant_tribe: Some("Elf".to_string()),
                commitment: 0.8,
                tribes: Vec::new(),
            },
            mana_ramp: ManaRampFeature {
                dork_count: 8,
                commitment: 0.6,
                ..Default::default()
            },
            ..Default::default()
        };
        let snapshot = derive_snapshot(&features);
        assert_eq!(
            snapshot.tempo_class,
            TempoClass::Ramp,
            "ramp+tribal hybrid should read as Ramp"
        );
    }

    #[test]
    fn high_aristocrats_commitment_picks_midrange_tempo() {
        let features = DeckFeatures {
            aristocrats: crate::features::aristocrats::AristocratsFeature {
                outlet_count: 3,
                free_outlet_count: 3,
                death_trigger_count: 3,
                fodder_source_count: 5,
                commitment: 0.9,
                outlet_names: vec!["Outlet A".to_string()],
                death_trigger_names: vec!["Trigger A".to_string()],
            },
            ..Default::default()
        };
        let snapshot = derive_snapshot(&features);
        assert_eq!(
            snapshot.tempo_class,
            TempoClass::Midrange,
            "high-commitment aristocrats deck should pick Midrange tempo"
        );
    }

    #[test]
    fn high_control_commitment_picks_control_tempo() {
        let features = DeckFeatures {
            control: crate::features::control::ControlFeature {
                counterspell_count: 4,
                spot_removal_count: 6,
                commitment: 0.9,
                reactive_tempo: 0.7,
                instant_count: 10,
                reactive_instant_ratio: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        let snapshot = derive_snapshot(&features);
        assert_eq!(
            snapshot.tempo_class,
            TempoClass::Control,
            "high-commitment control deck should pick Control tempo"
        );
    }

    #[test]
    fn control_commitment_delays_threats() {
        let baseline = derive_snapshot(&DeckFeatures::default());

        let features = DeckFeatures {
            control: crate::features::control::ControlFeature {
                counterspell_count: 4,
                spot_removal_count: 6,
                commitment: 0.9,
                reactive_tempo: 0.7,
                ..Default::default()
            },
            ..Default::default()
        };
        let control_snapshot = derive_snapshot(&features);

        // Turns 3 and 4 (indices 2 and 3) should be reduced by 1.
        assert!(
            control_snapshot.expected_threats[2] < baseline.expected_threats[2]
                || control_snapshot.expected_threats[2] == 0,
            "turn 3 threats should be delayed by control commitment"
        );
        assert!(
            control_snapshot.expected_threats[3] < baseline.expected_threats[3]
                || control_snapshot.expected_threats[3] == 0,
            "turn 4 threats should be delayed by control commitment"
        );
    }
}
