//! Hand-authored combo-line registry. The skeleton ships with one stub line
//! to verify end-to-end wiring; real cEDH lines (Thoracle/Consult, Heliod/Ballista,
//! Kiki/Twin, etc.) populate this registry in a follow-up phase as engine
//! card coverage stabilises.
//!
//! Stub choice rationale: Kiki-Jiki + Restoration Angel was evaluated first
//! (verified via `jq '."kiki-jiki, mirror breaker"' client/public/card-data.json`),
//! but both cards have complex triggered/activated abilities that are not yet
//! fully parsed. The synthetic single-piece stub is used instead so the wiring
//! proof-of-life is not gated on card-parsing coverage.

use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use crate::combo::detection::{ComboDetector, DefaultComboDetector};
use crate::combo::line::{
    CardPredicate, ComboLine, ComboLineId, ComboPiece, ComboReachability, WinKind,
};

pub struct ComboRegistry {
    lines: Vec<ComboLine>,
    detector: Box<dyn ComboDetector>,
}

impl Default for ComboRegistry {
    fn default() -> Self {
        Self {
            lines: vec![stub_line()],
            detector: Box::new(DefaultComboDetector),
        }
    }
}

impl ComboRegistry {
    /// Returns all combo lines that are reachable (this turn or next turn) for
    /// the given AI player. Lines that are `NotReachable` are filtered out.
    pub fn reachable_lines(
        &self,
        state: &GameState,
        ai: PlayerId,
    ) -> Vec<(ComboLineId, ComboReachability)> {
        self.lines
            .iter()
            .map(|line| (line.id, self.detector.assess(state, line, ai)))
            .filter(|(_, r)| !matches!(r, ComboReachability::NotReachable))
            .collect()
    }

    pub fn lines(&self) -> &[ComboLine] {
        &self.lines
    }
}

/// Skeleton-only stub. **Not a real cEDH combo.** Populates the registry
/// with one line so policy wiring can be exercised end-to-end. Real combos
/// land in a follow-up phase as engine card coverage stabilises.
fn stub_line() -> ComboLine {
    ComboLine {
        id: ComboLineId(0),
        name: "skeleton stub (not a real combo)",
        pieces: vec![ComboPiece::OnBattlefield(CardPredicate::NameEquals(
            "__cedh_stub_test_creature__",
        ))],
        mana_cost: ManaCost::NoCost,
        action_sequence: Vec::new(),
        win_kind: WinKind::LethalDamage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_returns_no_reachable_lines() {
        let state = GameState::new_two_player(0);
        let reg = ComboRegistry::default();
        assert_eq!(reg.reachable_lines(&state, PlayerId(0)).len(), 0);
    }

    #[test]
    fn registry_exposes_one_stub_line() {
        let reg = ComboRegistry::default();
        assert_eq!(reg.lines().len(), 1);
        assert_eq!(reg.lines()[0].id, ComboLineId(0));
    }
}
