//! Coverage plan: CR 506–511 Combat.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "507.1",
        section: 507,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "Beginning of combat step. Deferred: only asserts the setup-chosen phase until the \
                runner advances into combat.",
    },
    CoveragePlanEntry {
        // CR 508.1: the active player declares attackers (a TBA, no stack).
        rule: "508.1",
        section: 508,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["attacker_declared"],
        notes: "Declare attackers. Deferred: needs a DeclareAttackers runner step; the \
                attacker_declared assertion reads live CombatState once one exists.",
    },
    CoveragePlanEntry {
        // CR 509.1: the defending player declares blockers (a TBA, no stack).
        rule: "509.1",
        section: 509,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["blocker_declared"],
        notes: "Declare blockers. Deferred: needs a DeclareBlockers runner step; the \
                blocker_declared assertion reads live CombatState once one exists.",
    },
    CoveragePlanEntry {
        // CR 510.1: combat damage is assigned and dealt (a TBA).
        rule: "510.1",
        section: 510,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_damage", "player_life"],
        notes: "Combat damage step. Deferred: needs attacker/blocker declaration + a \
                combat-damage runner step before damage/life can be asserted.",
    },
    CoveragePlanEntry {
        rule: "511.1",
        section: 511,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "End of combat step. Deferred: needs advance to the end-of-combat step.",
    },
];
