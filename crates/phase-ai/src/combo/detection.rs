//! Combo reachability assessment over a `GameState`. The structural detector
//! walks `ComboLine::pieces`, matches them against the AI player's zones,
//! computes mana shortfall, and resolves the line's `action_sequence` into
//! concrete `GameAction` values by binding each `ComboStep` predicate to the
//! matching object on the AI's battlefield/hand.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use crate::combo::line::{CardPredicate, ComboLine, ComboPiece, ComboReachability, ComboStep};

pub trait ComboDetector: Send + Sync {
    fn assess(&self, state: &GameState, line: &ComboLine, ai: PlayerId) -> ComboReachability;
}

/// Structural detector. Reuses existing zone-iteration helpers:
/// - `state.players[ai.0 as usize].hand` / `.graveyard` / `.library` for
///   off-battlefield pieces.
/// - `state.battlefield` filtered by `controller == ai` for on-board pieces.
/// - `crate::zone_eval::available_mana(state, ai)` for mana shortfall.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralComboDetector;

impl ComboDetector for StructuralComboDetector {
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
            // Resolve each ComboStep against state to produce concrete
            // GameAction values. Targets are intentionally left empty —
            // ComboLinePolicy fires as a prior-boost *before* target
            // selection, and the engine's subsequent target-prompt flow
            // handles target choice independently.
            let required_actions = resolve_action_sequence(&line.action_sequence, state, ai);
            ComboReachability::ReachableThisTurn {
                missing_mana: shortfall as u8,
                required_actions,
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

pub(crate) fn piece_present(piece: &ComboPiece, state: &GameState, ai: PlayerId) -> bool {
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

/// Resolves each `ComboStep` to a concrete `GameAction` by binding the
/// step's predicate to the first matching object on the AI player's
/// battlefield (for `Activate`) or hand (for `Cast`). Steps whose source
/// object cannot be located are dropped from the resolved sequence — they
/// would have already caused the line to fall into the `NotReachable` /
/// `ReachableNextTurn` branches via the piece check above.
fn resolve_action_sequence(
    sequence: &[ComboStep],
    state: &GameState,
    ai: PlayerId,
) -> Vec<GameAction> {
    sequence
        .iter()
        .filter_map(|step| match step {
            ComboStep::Activate {
                predicate,
                ability_index,
            } => find_battlefield_object(state, ai, predicate).map(|source_id| {
                GameAction::ActivateAbility {
                    source_id,
                    ability_index: *ability_index as usize,
                }
            }),
            ComboStep::Cast { predicate } => {
                find_hand_object(state, ai, predicate).map(|object_id| {
                    let card_id = state.objects.get(&object_id).map(|o| o.card_id);
                    card_id.map(|card_id| GameAction::CastSpell {
                        object_id,
                        card_id,
                        targets: Vec::new(),
                    })
                })?
            }
        })
        .collect()
}

fn find_battlefield_object(
    state: &GameState,
    ai: PlayerId,
    pred: &CardPredicate,
) -> Option<ObjectId> {
    state.battlefield.iter().copied().find(|&id| {
        state
            .objects
            .get(&id)
            .is_some_and(|obj| obj.controller == ai && matches_predicate(pred, &obj.name))
    })
}

fn find_hand_object(state: &GameState, ai: PlayerId, pred: &CardPredicate) -> Option<ObjectId> {
    state.players[ai.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|&id| matches_in_zone(pred, state, id))
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
        let r = StructuralComboDetector.assess(&s, &line, PlayerId(0));
        assert!(matches!(r, ComboReachability::NotReachable));
    }

    #[test]
    fn reachable_this_turn_resolves_action_sequence_into_required_actions() {
        use crate::combo::line::ComboStep;
        use engine::game::zones::create_object;
        use engine::types::actions::GameAction;
        use engine::types::card_type::CoreType;
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut state = empty_state();
        // Two untapped Lands → available_mana == 2.
        for i in 0..2 {
            let land_id = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&land_id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        let src_id = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Resolvable Source".to_string(),
            Zone::Battlefield,
        );

        let line = ComboLine {
            id: ComboLineId(7),
            name: "resolve test",
            pieces: vec![ComboPiece::OnBattlefield(CardPredicate::NameEquals(
                "Resolvable Source",
            ))],
            mana_cost: ManaCost::Cost {
                shards: Vec::new(),
                generic: 2,
            },
            action_sequence: vec![ComboStep::Activate {
                predicate: CardPredicate::NameEquals("Resolvable Source"),
                ability_index: 3,
            }],
            win_kind: WinKind::LethalDamage,
        };

        match StructuralComboDetector.assess(&state, &line, PlayerId(0)) {
            ComboReachability::ReachableThisTurn {
                missing_mana,
                required_actions,
            } => {
                assert_eq!(missing_mana, 0);
                assert_eq!(required_actions.len(), 1);
                match &required_actions[0] {
                    GameAction::ActivateAbility {
                        source_id,
                        ability_index,
                    } => {
                        assert_eq!(*source_id, src_id);
                        assert_eq!(*ability_index, 3);
                    }
                    other => panic!("expected ActivateAbility, got {other:?}"),
                }
            }
            other => panic!("expected ReachableThisTurn, got {other:?}"),
        }
    }
}
