use crate::types::ability::{
    ChoiceType, Comparator, ControllerRef, Effect, EffectError, EffectKind, GuessSubject,
    ObjectScope, QuantityExpr, QuantityRef, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 608.2d + CR 608.2e: An opponent / the defending player guesses a value the
/// controller committed, or a proposition about game state, during resolution.
///
/// This resolver ONLY raises the interactive `WaitingFor::OpponentGuess`
/// round-trip and computes the legal option set (and, for a proposition, its
/// resolved truth). It does NOT install a continuation: because
/// `WaitingFor::OpponentGuess` is a member of `waits_for_resolution_choice`, the
/// generic chain walker auto-stashes `ability.sub_ability` onto
/// `pending_continuation` once this resolver returns. The answer handler
/// (`engine_resolution_choices.rs`) stamps the correct/incorrect `GuessOutcome`
/// onto that stashed chain via `EffectOutcomeSignal::Guessed` and drains it.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (guesser_ref, subject) = match &ability.effect {
        Effect::OpponentGuess { guesser, subject } => (guesser.clone(), subject.as_ref().clone()),
        _ => return Ok(()),
    };

    // CR 608.2e: resolve the guesser (a player other than the controller).
    let Some(guesser) = resolve_guesser(state, guesser_ref, ability) else {
        // CR 609.3: no eligible guesser — the guess does nothing and neither
        // rider fires (guess_outcome stays None).
        return Ok(());
    };

    let (options, choice_type, proposition_truth) = match &subject {
        GuessSubject::CommittedChoice { choice_type } => {
            // CR 608.2d: the guesser may name ANY printed value; a used number is
            // a wrong guess, not an illegal choice. So enumerate the full printed
            // domain directly from `choice_type` (ignoring `distinctness`), NOT
            // via the history-subtracting `compute_options`.
            let options = match choice_type {
                ChoiceType::NumberRange { min, max, .. } => {
                    (*min..=*max).map(|n| n.to_string()).collect::<Vec<_>>()
                }
                // Other committed-choice domains route through the shared option
                // enumerator's printed-domain semantics. No printed card uses a
                // non-number committed guess yet; fall back to an empty set so the
                // no-op guard below fires (CR 609.3) rather than wedging.
                _ => Vec::new(),
            };
            // CR 609.3: with nothing committed on the source there is nothing to
            // guess against — do nothing, leaving guess_outcome None.
            let has_commit = state.objects.get(&ability.source_id).is_some_and(|o| {
                o.chosen_attributes
                    .iter()
                    .any(|a| matches!(a, crate::types::ability::ChosenAttribute::Number(_)))
            });
            if options.is_empty() || !has_commit {
                return Ok(());
            }
            (options, choice_type.clone(), None)
        }
        GuessSubject::Proposition {
            lhs,
            comparator,
            rhs,
        } => {
            // CR 609.3: a proposition about a chosen referent (Seventh Doctor's
            // "that card") needs that referent to exist. When the controller had
            // no card to choose (empty hand), the chain has no object target, so
            // the guess does nothing and `Investigate` fires downstream.
            if references_target_scope(lhs) || references_target_scope(rhs) {
                let has_object_target = ability
                    .targets
                    .iter()
                    .any(|t| matches!(t, TargetRef::Object(_)));
                if !has_object_target {
                    return Ok(());
                }
            }
            // CR 608.2d: resolve the proposition's truth NOW, while the resolving
            // ability's targets are in scope (`ObjectScope::Target` reads the
            // chosen card). The guesser answers a yes/no about this truth.
            let lhs_val = crate::game::quantity::resolve_quantity_with_targets(state, lhs, ability);
            let rhs_val = crate::game::quantity::resolve_quantity_with_targets(state, rhs, ability);
            let truth = comparator.evaluate(lhs_val, rhs_val);
            let options = proposition_labels(*comparator);
            let choice_type = ChoiceType::Labeled {
                options: options.clone(),
            };
            (options, choice_type, Some(truth))
        }
    };

    if options.is_empty() {
        return Ok(());
    }

    state.waiting_for = WaitingFor::OpponentGuess {
        player: guesser,
        options,
        choice_type,
        source_id: ability.source_id,
        proposition_truth,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::OpponentGuess,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 608.2e: Resolve the guessing player from a `ControllerRef`.
fn resolve_guesser(
    state: &GameState,
    guesser: ControllerRef,
    ability: &ResolvedAbility,
) -> Option<PlayerId> {
    match guesser {
        ControllerRef::DefendingPlayer => {
            crate::game::combat::defending_player_for_attacker(state, ability.source_id)
        }
        ControllerRef::ChosenPlayer { index } => {
            ability.chosen_players.get(index as usize).copied()
        }
        ControllerRef::SourceChosenPlayer => ability.chosen_players.first().copied(),
        ControllerRef::Opponent => {
            let mut opponents = crate::game::players::opponents(state, ability.controller);
            (opponents.len() == 1).then(|| opponents.remove(0))
        }
        _ => None,
    }
}

/// CR 608.2b + CR 609.3: Whether a `QuantityExpr` (possibly composite) reads an
/// `ObjectScope::Target` referent — the chosen card for a
/// `GuessSubject::Proposition`. Recurses into all `QuantityExpr` wrappers so
/// composite expressions such as "that card's mana value plus 1"
/// (`Offset { Ref(ObjectManaValue{Target}), 1 }`) are detected correctly.
/// When no object target exists (empty hand), the proposition is skipped
/// (CR 609.3: an effect that would reference a non-existent object does nothing).
fn references_target_scope(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => quantity_ref_targets(qty),
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::DivideRounded { inner, .. } => references_target_scope(inner),
        QuantityExpr::Offset { inner, .. } => references_target_scope(inner),
        QuantityExpr::ClampMin { inner, .. } => references_target_scope(inner),
        QuantityExpr::Multiply { inner, .. } => references_target_scope(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(references_target_scope),
        QuantityExpr::UpTo { max } => references_target_scope(max),
        QuantityExpr::Power { exponent, .. } => references_target_scope(exponent),
        QuantityExpr::Difference { left, right } => {
            references_target_scope(left) || references_target_scope(right)
        }
        QuantityExpr::Max { exprs } => exprs.iter().any(references_target_scope),
    }
}

/// CR 608.2b: Whether a `QuantityRef` leaf directly reads an
/// `ObjectScope::Target` referent (mana value, power, or toughness of the
/// chosen card). Extended as new `ObjectScope::Target` ref variants are added.
fn quantity_ref_targets(qty: &QuantityRef) -> bool {
    matches!(
        qty,
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::Target
        } | QuantityRef::Power {
            scope: ObjectScope::Target
        } | QuantityRef::Toughness {
            scope: ObjectScope::Target
        }
    )
}

/// CR 608.2d: Human-readable yes/no labels for a proposition guess. `labels[0]`
/// is the affirmative reading (the comparator-true answer); the guesser is
/// correct when the boolean reading of its choice equals the resolved truth.
fn proposition_labels(comparator: Comparator) -> Vec<String> {
    let (yes, no) = match comparator {
        Comparator::GT => ("greater", "not greater"),
        Comparator::GE => ("greater or equal", "less"),
        Comparator::LT => ("less", "not less"),
        Comparator::LE => ("less or equal", "greater"),
        Comparator::EQ => ("equal", "not equal"),
        Comparator::NE => ("not equal", "equal"),
    };
    vec![yes.to_string(), no.to_string()]
}

/// CR 608.2d: Compute whether a guesser's chosen option string is correct.
/// Shared single authority used by the answer handler.
///
/// For a proposition (`proposition_truth = Some(truth)`), the guesser is correct
/// when its option matches the affirmative label iff `truth`. For a committed
/// choice (`proposition_truth = None`), the guesser is correct when its named
/// number equals the source's LAST committed `ChosenAttribute::Number`
/// (CR 608.2c "the last chosen value" — read within this same resolution, where
/// the choose instruction precedes the guess instruction in the one ability).
pub(crate) fn guess_is_correct(
    state: &GameState,
    source_id: ObjectId,
    options: &[String],
    choice: &str,
    proposition_truth: Option<bool>,
) -> bool {
    match proposition_truth {
        Some(truth) => {
            let chose_affirmative = options.first().is_some_and(|aff| aff == choice);
            chose_affirmative == truth
        }
        None => {
            // CR 608.2c: read THE LAST committed number (the one chosen this
            // resolution), not the first — the Toymaker persists across upkeeps.
            // The choose instruction precedes the guess within this single
            // ability, so this is an in-resolution back-reference, not a
            // CR 607.2d link between two distinct printed abilities.
            let committed = state.objects.get(&source_id).and_then(|o| {
                o.chosen_attributes.iter().rev().find_map(|a| match a {
                    crate::types::ability::ChosenAttribute::Number(n) => Some(*n as i32),
                    _ => None,
                })
            });
            choice
                .parse::<i32>()
                .ok()
                .is_some_and(|guessed| committed == Some(guessed))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::game_state::GameState;

    /// CR 608.2b + CR 609.3: `references_target_scope` must recurse into
    /// `QuantityExpr::Offset` so a composed expression like "that card's mana
    /// value plus 1" is correctly detected as target-dependent.
    ///
    /// The prior bug: `Offset` was not traversed — only bare `QuantityExpr::Ref`
    /// was matched — so a composed lhs was treated as target-independent, and
    /// `OpponentGuess` could be raised with a false 0-based truth value even when
    /// no object target existed.
    #[test]
    fn references_target_scope_detects_offset_wrapping_target_ref() {
        let composed = QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            }),
            offset: 1,
        };
        assert!(
            references_target_scope(&composed),
            "Offset wrapping a Target-scope ref must be detected as target-dependent"
        );
    }

    /// CR 608.2b + CR 609.3: `references_target_scope` must recurse into
    /// `QuantityExpr::Sum` to detect a Target-scope ref in any summand.
    #[test]
    fn references_target_scope_detects_sum_containing_target_ref() {
        let target_ref = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Target,
            },
        };
        let sum = QuantityExpr::Sum {
            exprs: vec![QuantityExpr::Fixed { value: 1 }, target_ref],
        };
        assert!(
            references_target_scope(&sum),
            "Sum containing a Target-scope ref must be detected as target-dependent"
        );
    }

    /// CR 608.2b: A non-Target ObjectScope ref (`Source`) must NOT be flagged
    /// as target-dependent by `references_target_scope`.
    #[test]
    fn references_target_scope_ignores_source_scope() {
        let source_ref = QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Source,
            },
        };
        assert!(
            !references_target_scope(&source_ref),
            "A Source-scope ref must not be treated as target-dependent"
        );
    }

    /// CR 608.2d + CR 609.3: When the proposition's `lhs` is a composed
    /// `QuantityExpr` (Offset wrapping ObjectManaValue{Target}) and the
    /// resolving ability carries no object target (simulating the empty-hand /
    /// no-card-chosen path), the resolver must skip `OpponentGuess` entirely —
    /// the guess does nothing and the no-guess tail proceeds.
    ///
    /// This test would have FAILED on the old `references_target_scope`
    /// implementation (which only matched bare `QuantityExpr::Ref` at top level),
    /// because the composed `lhs` would have been treated as target-independent
    /// and a guess would have been raised against a false 0-based proposition.
    #[test]
    fn resolve_skips_guess_for_composed_target_scope_proposition_with_no_object_target() {
        let mut state = GameState::default();
        let mut events = Vec::new();

        // Composed lhs: "that card's mana value plus 1" — references ObjectScope::Target
        // inside an Offset wrapper. The old code missed this.
        let composed_lhs = QuantityExpr::Offset {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            }),
            offset: 1,
        };

        let ability = ResolvedAbility::new(
            Effect::OpponentGuess {
                guesser: ControllerRef::Opponent,
                subject: Box::new(GuessSubject::Proposition {
                    lhs: composed_lhs,
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }),
            },
            vec![], // No object target — simulates the empty-hand / no-card-chosen path
            crate::types::identifiers::ObjectId(1),
            crate::types::player::PlayerId(0),
        );

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok(), "resolve must not error: {:?}", result);
        assert!(
            !matches!(state.waiting_for, WaitingFor::OpponentGuess { .. }),
            "a composed target-scope proposition with no object target must not raise \
             OpponentGuess — the guess does nothing (CR 609.3); got: {:?}",
            state.waiting_for
        );
    }
}
