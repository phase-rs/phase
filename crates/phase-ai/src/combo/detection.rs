//! Combo reachability assessment over a `GameState`. The structural detector
//! walks `ComboLine::pieces`, matches them against the AI player's zones,
//! checks affordability via the engine's color-accurate auto-tap primitive,
//! and resolves the line's `action_sequence` into concrete `GameAction` values
//! by binding each `ComboStep` predicate to the matching object on the AI's
//! battlefield/hand.

use engine::game::casting::can_pay_cost_after_auto_tap;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use crate::combo::line::{CardPredicate, ComboLine, ComboPiece, ComboReachability, ComboStep};
use engine::types::game_state::CastPaymentMode;

pub trait ComboDetector: Send + Sync {
    fn assess(&self, state: &GameState, line: &ComboLine, ai: PlayerId) -> ComboReachability;
}

/// Structural detector. Reuses existing zone-iteration helpers:
/// - `state.players[ai.0 as usize].hand` / `.graveyard` / `.library` for
///   off-battlefield pieces.
/// - `state.battlefield` filtered by `controller == ai` for on-board pieces.
/// - Affordability is delegated to the engine's color-accurate primitive
///   `engine::game::casting::can_pay_cost_after_auto_tap`, which simulates
///   mana-ability activation (auto-tap) on a clone of the state.
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
            // All pieces present. Check affordability.
            // CR 601.2g + CR 601.2h: a line is castable this turn only if the AI can pay
            // the cost-bearing piece's mana cost after activating mana abilities (auto-tap).
            // Delegates to the engine's color-accurate affordability primitive, which also
            // enforces summoning sickness (CR 302.6).
            let affordable = match &line.mana_cost {
                ManaCost::NoCost | ManaCost::SelfManaCost | ManaCost::SelfManaValue => true,
                ManaCost::Cost { .. } => cost_bearing_source(line, state, ai).is_some_and(|src| {
                    can_pay_cost_after_auto_tap(state, ai, src, &line.mana_cost)
                }),
            };
            if affordable {
                // Resolve each ComboStep against state to produce concrete
                // GameAction values. Targets are intentionally left empty —
                // ComboLinePolicy fires as a prior-boost *before* target
                // selection, and the engine's subsequent target-prompt flow
                // handles target choice independently.
                let required_actions = resolve_action_sequence(&line.action_sequence, state, ai);
                ComboReachability::ReachableThisTurn {
                    missing_mana: 0,
                    required_actions,
                }
            } else {
                ComboReachability::NotReachable
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
    // Index defensively: a player may have been eliminated/removed, so a raw
    // `state.players[ai.0]` could panic. A missing player means no piece.
    let player = match state.players.get(ai.0 as usize) {
        Some(p) => p,
        None => return false,
    };
    match piece {
        ComboPiece::InHand(pred) => player
            .hand
            .iter()
            .any(|&id| matches_in_zone(pred, state, id)),
        ComboPiece::OnBattlefield(pred) => state.battlefield.iter().any(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|obj| obj.controller == ai && matches_predicate(pred, &obj.name))
        }),
        ComboPiece::InGraveyard(pred) => player
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

                        payment_mode: CastPaymentMode::Auto,
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
    state
        .players
        .get(ai.0 as usize)?
        .hand
        .iter()
        .copied()
        .find(|&id| matches_in_zone(pred, state, id))
}

/// Locates the object that bears the line's mana cost — the source the engine's
/// affordability primitive prioritizes (and deprioritizes from tapping for its
/// own cost). Matches the line's first `ComboStep`: an `Activate` step bears the
/// cost on its battlefield source; a `Cast` step bears it on the hand object.
fn cost_bearing_source(line: &ComboLine, state: &GameState, ai: PlayerId) -> Option<ObjectId> {
    match line.action_sequence.first() {
        Some(ComboStep::Activate { predicate, .. }) => {
            find_battlefield_object(state, ai, predicate)
        }
        Some(ComboStep::Cast { predicate }) => find_hand_object(state, ai, predicate),
        None => None,
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
        // Two untapped Forests → GG, which pays the generic {2}.
        for i in 0..2 {
            let land_id = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
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
