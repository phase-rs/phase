//! Step-kind catalog — documents declarative runner actions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepCatalogEntry {
    pub action: &'static str,
    pub description: &'static str,
    pub cr_hooks: &'static [&'static str],
}

pub const STEP_CATALOG: &[StepCatalogEntry] = &[
    StepCatalogEntry {
        action: "pass_priority",
        description: "Pass priority once for the current player (CR 117).",
        cr_hooks: &["117.3", "117.4"],
    },
    StepCatalogEntry {
        action: "pass_both",
        description: "Pass priority for both players in succession.",
        cr_hooks: &["117.4"],
    },
    StepCatalogEntry {
        action: "mark_damage",
        description: "Add marked damage to a named creature handle, then check SBAs.",
        cr_hooks: &["120.3", "704.5g"],
    },
    StepCatalogEntry {
        action: "set_life",
        description: "Set a player's life total, then check SBAs.",
        cr_hooks: &["119", "704.5a"],
    },
    StepCatalogEntry {
        action: "damage_player",
        description: "Reduce a player's life by amount (resolved damage), then check SBAs.",
        cr_hooks: &["120.4", "704.5a"],
    },
    StepCatalogEntry {
        action: "advance_until_stack_empty",
        description: "Resolve until the stack is empty.",
        cr_hooks: &["608", "405"],
    },
    StepCatalogEntry {
        action: "check_sbas",
        description: "Force an SBA check via priority pass when legal.",
        cr_hooks: &["704.3"],
    },
];

pub fn step_by_action(action: &str) -> Option<&'static StepCatalogEntry> {
    STEP_CATALOG.iter().find(|s| s.action == action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_actions_unique() {
        let mut actions: Vec<_> = STEP_CATALOG.iter().map(|s| s.action).collect();
        let before = actions.len();
        actions.sort_unstable();
        actions.dedup();
        assert_eq!(before, actions.len());
    }
}
