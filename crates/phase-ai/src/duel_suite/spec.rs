//! Static matchup registry. Every `--matchup NAME` invocation resolves via
//! `find_matchup(name)`; `--suite` iterates `MATCHUPS` and runs every entry.
//!
//! **Preservation contract:** the 16 legacy matchup IDs (see
//! `matchup_ids_preserved` in `tests.rs`) MUST remain resolvable and MUST map
//! to the same decks + labels as before the registry refactor. Adding new
//! matchups is allowed; renaming or removing legacy IDs is not.

use super::inline_decks::{
    deck_azorius_control, deck_black_midrange, deck_blue_control, deck_green_midrange,
    deck_gruul_prowess, deck_izzet_delver, deck_mono_red_prowess, deck_red_aggro,
    deck_white_weenie,
};
use super::{DeckRef, Expected, FeatureKind, MatchupSpec};

/// Legacy mirror tolerance retained in the static data shape. Runtime mirror
/// classification uses a Wilson 95% confidence interval containing 0.5: quick
/// n=10 smoke runs are intentionally low-power, while the n=100 suite narrows
/// the false-fail band enough for the nightly gate.
const MIRROR_TOLERANCE: f32 = 0.15;

const fn snap(format: &'static str, file: &'static str) -> DeckRef {
    DeckRef::Snapshot { format, file }
}

const fn inline(label: &'static str, build: fn() -> Vec<String>) -> DeckRef {
    DeckRef::Inline { label, build }
}

pub static MATCHUPS: &[MatchupSpec] = &[
    // --- Legacy starter-deck matchups (preserved from pre-refactor ai_duel.rs) ---
    MatchupSpec {
        id: "red-vs-green",
        p0_label: "Red Aggro",
        p1_label: "Green Midrange",
        p0: inline("Red Aggro", deck_red_aggro),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[FeatureKind::AggroPressure],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "blue-vs-green",
        p0_label: "Blue Control",
        p1_label: "Green Midrange",
        p0: inline("Blue Control", deck_blue_control),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[FeatureKind::Control],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "red-vs-blue",
        p0_label: "Red Aggro",
        p1_label: "Blue Control",
        p0: inline("Red Aggro", deck_red_aggro),
        p1: inline("Blue Control", deck_blue_control),
        exercises: &[FeatureKind::AggroPressure, FeatureKind::Control],
        expected: Expected::Triangle {
            p0_winrate_min: 0.0,
            p0_winrate_max: 1.0,
        },
    },
    MatchupSpec {
        id: "black-vs-green",
        p0_label: "Black Midrange",
        p1_label: "Green Midrange",
        p0: inline("Black Midrange", deck_black_midrange),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "white-vs-red",
        p0_label: "White Weenie",
        p1_label: "Red Aggro",
        p0: inline("White Weenie", deck_white_weenie),
        p1: inline("Red Aggro", deck_red_aggro),
        exercises: &[FeatureKind::AggroPressure],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "black-vs-blue",
        p0_label: "Black Midrange",
        p1_label: "Blue Control",
        p0: inline("Black Midrange", deck_black_midrange),
        p1: inline("Blue Control", deck_blue_control),
        exercises: &[FeatureKind::Control],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "red-mirror",
        p0_label: "Red Aggro (P0)",
        p1_label: "Red Aggro (P1)",
        p0: inline("Red Aggro", deck_red_aggro),
        p1: inline("Red Aggro", deck_red_aggro),
        exercises: &[FeatureKind::AggroPressure],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "green-mirror",
        p0_label: "Green Mid (P0)",
        p1_label: "Green Mid (P1)",
        p0: inline("Green Midrange", deck_green_midrange),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "blue-mirror",
        p0_label: "Blue Ctrl (P0)",
        p1_label: "Blue Ctrl (P1)",
        p0: inline("Blue Control", deck_blue_control),
        p1: inline("Blue Control", deck_blue_control),
        exercises: &[FeatureKind::Control],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    // --- Legacy metagame matchups (preserved from pre-refactor ai_duel.rs) ---
    MatchupSpec {
        id: "azorius-vs-prowess",
        p0_label: "Azorius Control",
        p1_label: "Mono-Red Prowess",
        p0: inline("Azorius Control", deck_azorius_control),
        p1: inline("Mono-Red Prowess", deck_mono_red_prowess),
        exercises: &[
            FeatureKind::Control,
            FeatureKind::AggroPressure,
            FeatureKind::SpellslingerProwess,
        ],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "azorius-vs-gruul",
        p0_label: "Azorius Control",
        p1_label: "Gruul Prowess",
        p0: inline("Azorius Control", deck_azorius_control),
        p1: inline("Gruul Prowess", deck_gruul_prowess),
        exercises: &[
            FeatureKind::Control,
            FeatureKind::AggroPressure,
            FeatureKind::SpellslingerProwess,
        ],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "delver-vs-prowess",
        p0_label: "Izzet Delver",
        p1_label: "Mono-Red Prowess",
        p0: inline("Izzet Delver", deck_izzet_delver),
        p1: inline("Mono-Red Prowess", deck_mono_red_prowess),
        exercises: &[FeatureKind::SpellslingerProwess, FeatureKind::AggroPressure],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "azorius-vs-green",
        p0_label: "Azorius Control",
        p1_label: "Green Midrange",
        p0: inline("Azorius Control", deck_azorius_control),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[FeatureKind::Control],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "delver-vs-green",
        p0_label: "Izzet Delver",
        p1_label: "Green Midrange",
        p0: inline("Izzet Delver", deck_izzet_delver),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[FeatureKind::SpellslingerProwess],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "prowess-vs-green",
        p0_label: "Mono-Red Prowess",
        p1_label: "Green Midrange",
        p0: inline("Mono-Red Prowess", deck_mono_red_prowess),
        p1: inline("Green Midrange", deck_green_midrange),
        exercises: &[FeatureKind::AggroPressure, FeatureKind::SpellslingerProwess],
        expected: Expected::Open,
    },
    MatchupSpec {
        id: "prowess-mirror",
        p0_label: "RDW Prowess (P0)",
        p1_label: "RDW Prowess (P1)",
        p0: inline("Mono-Red Prowess", deck_mono_red_prowess),
        p1: inline("Mono-Red Prowess", deck_mono_red_prowess),
        exercises: &[FeatureKind::AggroPressure, FeatureKind::SpellslingerProwess],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    // --- Snapshot-based mirrors for feature-kind coverage ---
    MatchupSpec {
        id: "landfall-mirror",
        p0_label: "Mono-Green Landfall (P0)",
        p1_label: "Mono-Green Landfall (P1)",
        p0: snap("standard", "mono-green-landfall.json"),
        p1: snap("standard", "mono-green-landfall.json"),
        exercises: &[FeatureKind::Landfall, FeatureKind::ManaRamp],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "tokens-mirror",
        p0_label: "Selesnya Ouroboroid (P0)",
        p1_label: "Selesnya Ouroboroid (P1)",
        p0: snap("standard", "selesnya-ouroboroid.json"),
        p1: snap("standard", "selesnya-ouroboroid.json"),
        exercises: &[FeatureKind::TokensWide],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        // TODO(perf): greasefang-mirror games run notably longer than other
        // matchups (~160s vs <30s) because the deck's "Discard a card:
        // <redundant-effect>" activated abilities (Fleeting Spirit,
        // Iron-Shield Elf, Guardian of New Benalia) combine with Monument to
        // Endurance's discard-triggered draw to produce a card-neutral loop
        // that the AI's softmax scores net-positive. The
        // `pending_activations` guard + per-source per-turn activation cap
        // in `phase-ai/src/search.rs` bound the pathology (game completes
        // naturally), but the remaining slowness reflects a legitimate AI
        // search cost on the cluttered board. Fixing the underlying eval
        // pathology (penalize activations whose effect would be a no-op
        // against current state) is out of scope for the initial fix.
        id: "greasefang-mirror",
        p0_label: "Orzhov Greasefang (P0)",
        p1_label: "Orzhov Greasefang (P1)",
        p0: snap("pioneer", "orzhov-greasefang.json"),
        p1: snap("pioneer", "orzhov-greasefang.json"),
        exercises: &[FeatureKind::Aristocrats],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "affinity-mirror",
        p0_label: "Affinity (P0)",
        p1_label: "Affinity (P1)",
        p0: snap("modern", "affinity.json"),
        p1: snap("modern", "affinity.json"),
        // Modern Affinity is the canonical artifacts-matter list: an artifact-dense
        // board (Mox Opal, Arcbound Ravager, Mishra's Bauble, …) feeding
        // affinity-for-artifacts / improvise payoffs (Kappa Cannoneer, Metallic
        // Rebuke). It clears `artifacts::COMMITMENT_FLOOR`, so this matchup is the
        // gate's exercise of `ArtifactSynergyPolicy` (verified by
        // `affinity_mirror_deck_activates_artifact_synergy` below).
        exercises: &[
            FeatureKind::Artifacts,
            FeatureKind::PlusOneCounters,
            FeatureKind::AggroPressure,
        ],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "enchantress-mirror",
        p0_label: "Selesnya Enchantress (P0)",
        p1_label: "Selesnya Enchantress (P1)",
        p0: snap("pioneer", "selesnya-enchantress.json"),
        p1: snap("pioneer", "selesnya-enchantress.json"),
        // Selesnya Enchantress is the canonical enchantments-matter list:
        // enchantress / constellation payoffs (Eidolon of Blossoms, Setessan
        // Champion, Sythis, Enchantress's Presence) over an enchantment-dense
        // board. It clears `enchantments::COMMITMENT_FLOOR`, so this matchup is
        // the gate's exercise of `EnchantmentsPayoffPolicy` (verified by
        // `enchantress_mirror_deck_activates_enchantments_payoff` below).
        exercises: &[FeatureKind::Enchantments],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "elves-mirror",
        p0_label: "Elves (P0)",
        p1_label: "Elves (P1)",
        p0: snap("pauper", "elves.json"),
        p1: snap("pauper", "elves.json"),
        exercises: &[FeatureKind::Tribal],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "amulet-mirror",
        p0_label: "Amulet Titan (P0)",
        p1_label: "Amulet Titan (P1)",
        p0: snap("modern", "amulet-titan.json"),
        p1: snap("modern", "amulet-titan.json"),
        exercises: &[FeatureKind::ManaRamp, FeatureKind::Landfall],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "lands-mirror",
        p0_label: "Lands (P0)",
        p1_label: "Lands (P1)",
        p0: snap("legacy", "lands.json"),
        p1: snap("legacy", "lands.json"),
        exercises: &[FeatureKind::Landfall],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "niv-mirror",
        p0_label: "Niv to Light (P0)",
        p1_label: "Niv to Light (P1)",
        p0: snap("pioneer", "niv-to-light.json"),
        p1: snap("pioneer", "niv-to-light.json"),
        exercises: &[FeatureKind::Control],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "delver-snapshot-mirror",
        p0_label: "Izzet Delver snap (P0)",
        p1_label: "Izzet Delver snap (P1)",
        p0: snap("legacy", "izzet-delver.json"),
        p1: snap("legacy", "izzet-delver.json"),
        exercises: &[FeatureKind::SpellslingerProwess, FeatureKind::Control],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "boros-energy-mirror",
        p0_label: "Boros Energy (P0)",
        p1_label: "Boros Energy (P1)",
        p0: snap("modern", "boros-energy.json"),
        p1: snap("modern", "boros-energy.json"),
        exercises: &[FeatureKind::AggroPressure],
        expected: Expected::Mirror {
            tolerance: MIRROR_TOLERANCE,
        },
    },
    MatchupSpec {
        id: "landfall-vs-niv",
        p0_label: "Mono-Green Landfall",
        p1_label: "Niv to Light",
        p0: snap("standard", "mono-green-landfall.json"),
        p1: snap("pioneer", "niv-to-light.json"),
        exercises: &[FeatureKind::Landfall, FeatureKind::Control],
        expected: Expected::Open,
    },
];

pub fn all_matchups() -> &'static [MatchupSpec] {
    MATCHUPS
}

pub fn find_matchup(id: &str) -> Option<&'static MatchupSpec> {
    MATCHUPS.iter().find(|m| m.id == id)
}
