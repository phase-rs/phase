use serde::{Deserialize, Serialize};

/// Which printed contest is being adapted. Selects presentation only — the
/// timing, input, winner and lifecycle logic is identical for every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContestKind {
    /// Eye to Eye: "You and target creature's controller have a staring contest."
    Staring,
    /// Mouth to Mouth: "You and that player have a breath-holding contest."
    BreathHolding,
}
