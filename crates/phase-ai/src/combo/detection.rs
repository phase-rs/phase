//! Combo reachability assessment over a `GameState`. The default detector
//! is structural: walks `ComboLine::pieces`, matches them against the AI
//! player's zones, and computes mana shortfall.

use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use crate::combo::line::{CardPredicate, ComboLine, ComboPiece, ComboReachability};

pub trait ComboDetector: Send + Sync {
    fn assess(&self, state: &GameState, line: &ComboLine, ai: PlayerId) -> ComboReachability;
}

/// Default structural detector. Reuses existing zone-iteration helpers:
/// - `state.players[ai.0 as usize].hand` / `.graveyard` / `.library` for
///   off-battlefield pieces.
/// - `state.battlefield` filtered by `controller == ai` for on-board pieces.
/// - `crate::zone_eval::available_mana(state, ai)` for mana shortfall.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultComboDetector;

impl ComboDetector for DefaultComboDetector {
    fn assess(&self, state: &GameState, line: &ComboLine, ai: PlayerId) -> ComboReachability {
        let mut missing: Vec<ComboPiece> = Vec::new();
        for piece in &line.pieces {
            if !piece_present(piece, state, ai) {
                missing.push(piece.clone());
            }
        }

        if missing.is_empty() {
            // All pieces present. Check mana.
            let available = crate::zone_eval::available_mana(state, ai);
            let required = mana_cost_total(&line.mana_cost);
            let shortfall = required.saturating_sub(available as i32);
            if shortfall == 0 {
                ComboReachability::ReachableThisTurn {
                    missing_mana: 0,
                    // Phase 5 wires action_sequence -> required_actions
                    required_actions: Vec::new(),
                }
            } else {
                ComboReachability::ReachableThisTurn {
                    missing_mana: shortfall as u8,
                    required_actions: Vec::new(),
                }
            }
        } else if missing
            .iter()
            .all(|p| matches!(p, ComboPiece::InLibrary(_)))
        {
            // Pieces are tutorable but not in hand/board yet.
            ComboReachability::ReachableNextTurn {
                missing_pieces: missing,
            }
        } else {
            ComboReachability::NotReachable
        }
    }
}

fn piece_present(piece: &ComboPiece, state: &GameState, ai: PlayerId) -> bool {
    match piece {
        ComboPiece::InHand(pred) => state.players[ai.0 as usize]
            .hand
            .iter()
            .any(|&id| matches_in_zone(pred, state, id)),
        ComboPiece::OnBattlefield(pred) => state.battlefield.iter().any(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|obj| obj.controller == ai && matches_predicate(pred, &obj.name))
        }),
        ComboPiece::InGraveyard(pred) => state.players[ai.0 as usize]
            .graveyard
            .iter()
            .any(|&id| matches_in_zone(pred, state, id)),
        // InLibrary is treated as "tutorable, not yet present" — never returns true.
        // The reachability path elevates lines whose only-missing-pieces are InLibrary
        // to ReachableNextTurn so tutors get prior boosts.
        ComboPiece::InLibrary(_) => false,
    }
}

fn matches_in_zone(pred: &CardPredicate, state: &GameState, id: ObjectId) -> bool {
    state
        .objects
        .get(&id)
        .is_some_and(|obj| matches_predicate(pred, &obj.name))
}

fn matches_predicate(pred: &CardPredicate, name: &str) -> bool {
    match pred {
        CardPredicate::NameEquals(target) => name == *target,
    }
}

/// The MVP collapses colored + generic into a single integer cost; refine
/// when real combo lines need color-aware matching.
fn mana_cost_total(cost: &ManaCost) -> i32 {
    match cost {
        ManaCost::Cost { shards, generic } => (shards.len() as i32) + (*generic as i32),
        ManaCost::NoCost | ManaCost::SelfManaCost => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combo::line::{CardPredicate, ComboLine, ComboLineId, ComboPiece, WinKind};
    use engine::types::game_state::GameState;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;

    fn empty_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn one_piece_line() -> ComboLine {
        ComboLine {
            id: ComboLineId(999),
            name: "test stub",
            pieces: vec![ComboPiece::InHand(CardPredicate::NameEquals(
                "__test_piece__",
            ))],
            mana_cost: ManaCost::NoCost,
            action_sequence: Vec::new(),
            win_kind: WinKind::ImmediateLoss,
        }
    }

    #[test]
    fn empty_state_yields_not_reachable() {
        let s = empty_state();
        let line = one_piece_line();
        let r = DefaultComboDetector.assess(&s, &line, PlayerId(0));
        assert!(matches!(r, ComboReachability::NotReachable));
    }
}
