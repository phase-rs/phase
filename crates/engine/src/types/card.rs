use serde::{Deserialize, Serialize};

/// Stub card definition -- Phase 2 parser fills in the real structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardDefinition {
    pub name: String,
    pub mana_cost: Option<String>,
    pub type_line: String,
    pub oracle_text: Option<String>,
}
