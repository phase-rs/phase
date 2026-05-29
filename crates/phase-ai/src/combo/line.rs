//! Combo-line type system. Pure data — no game-state or registry coupling.
//! Detection logic lives in `combo/detection.rs`; the registry in `combo/registry.rs`.

use engine::types::actions::GameAction;
use engine::types::mana::ManaCost;

/// Stable identity for a registered combo line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComboLineId(pub u32);

/// A registered cEDH win condition. Hand-authored; the registry exposes
/// only the lines the engine + parser currently support cleanly.
#[derive(Debug, Clone)]
pub struct ComboLine {
    pub id: ComboLineId,
    pub name: &'static str,
    pub pieces: Vec<ComboPiece>,
    pub mana_cost: ManaCost,
    pub action_sequence: Vec<ComboStep>,
    pub win_kind: WinKind,
}

/// A required component of a combo line, located by zone + predicate.
/// Predicates are intentionally narrow for the skeleton — name-based matching
/// is acceptable here because combo lines are hand-authored. Structural
/// predicates can replace name matching once the AST coverage stabilises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComboPiece {
    InHand(CardPredicate),
    OnBattlefield(CardPredicate),
    InGraveyard(CardPredicate),
    /// Tutorable but not yet present — never returns `true` from zone checks.
    /// Lines whose only missing pieces are `InLibrary` are elevated to
    /// `ReachableNextTurn` by the detector.
    InLibrary(CardPredicate),
}

/// Narrow card predicate for the combo skeleton. Real combo content can
/// extend this to compose structural filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardPredicate {
    NameEquals(&'static str),
}

#[derive(Debug, Clone)]
pub enum ComboStep {
    Cast {
        predicate: CardPredicate,
    },
    Activate {
        predicate: CardPredicate,
        ability_index: u8,
    },
}

/// The manner in which a combo line achieves victory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinKind {
    /// CR 104.2 explicit win/loss effect (Thoracle / Laboratory Maniac).
    ImmediateLoss,
    /// Unbounded repetition (e.g., infinite combat, draw, or damage loop)
    /// achieving a win through an in-game condition.
    InfiniteLoop,
    /// Lethal damage or commander damage from the combo's resolution.
    LethalDamage,
}

/// Reachability assessment for a combo line against a game state.
#[derive(Debug, Clone)]
pub enum ComboReachability {
    NotReachable,
    ReachableThisTurn {
        missing_mana: u8,
        required_actions: Vec<GameAction>,
    },
    ReachableNextTurn {
        missing_pieces: Vec<ComboPiece>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_line_id_is_hashable_and_comparable() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(ComboLineId(1));
        s.insert(ComboLineId(2));
        s.insert(ComboLineId(1));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn combo_piece_eq_respects_zone() {
        assert_eq!(
            ComboPiece::InHand(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
            ComboPiece::InHand(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
        );
        assert_ne!(
            ComboPiece::InHand(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
            ComboPiece::OnBattlefield(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
        );
    }
}
