//! Shared helpers for walking an `AbilityDefinition`'s effect tree.
//!
//! `AbilityDefinition` composes a primary `effect` with an optional
//! `sub_ability` that itself has an `effect` plus another optional
//! `sub_ability`, forming a single-linked list. Feature detectors and
//! policies both need to classify the *set* of effects produced by an
//! ability (e.g., "does this ability both search the library and put a
//! land onto the battlefield?"), so they collect the chain into a flat
//! slice and iterate with `matches!`.
//!
//! Two branches of that definition are *conditional* rather than part of the
//! unconditional chain: `else_ability` (the CR 608.2c "Otherwise, ..." leg) and
//! `mode_abilities` (the CR 700.2 modal alternatives). Whether they belong in
//! the walk depends on the question being asked, which is what [`AbilityScope`]
//! names — see its docs. [`collect_scoped_effects`] is the single authority for
//! both walks; `collect_chain_effects` is the unconditional shorthand.
//!
//! Keep this module small — it is a single building block shared across
//! `features/*` and `policies/*`.

use engine::types::ability::{AbilityDefinition, Effect};

/// Which part of an ability tree a classification question is asking about.
///
/// The distinction is a decision-boundary one, not a convenience one:
///
/// * [`AbilityScope::Unconditional`] answers *"does resolving this ability as
///   already announced produce effect X?"* — the walk a LIVE per-action policy
///   needs. CR 601.2b makes mode selection a distinct step of announcing a
///   spell, so at `CastSpell` time no mode has been chosen yet and a modal
///   branch must not be credited to the cast.
/// * [`AbilityScope::Potential`] answers *"can this card ever produce effect
///   X?"* — the walk DECK-TIME detection needs, where every branch the card
///   could take is in scope.
///
/// A typed scope rather than a `bool` so each call site states which question
/// it is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbilityScope {
    /// `effect` plus the `sub_ability` chain — everything that happens no
    /// matter which branch or mode is taken.
    Unconditional,
    /// The full tree: `Unconditional` plus the `else_ability` branch (CR
    /// 603.4) and every entry of `mode_abilities` (CR 700.2), recursively.
    Potential,
}

/// Walk an ability tree at `scope`, returning borrowed effect references in
/// traversal order.
///
/// This is the mode-aware authority both deck-time detection and live policy
/// classification share; they differ only in the `scope` they pass, so the two
/// can never drift apart on which branches exist.
pub(crate) fn collect_scoped_effects(
    ability: &AbilityDefinition,
    scope: AbilityScope,
) -> Vec<&Effect> {
    let mut effects: Vec<&Effect> = Vec::new();
    push_scoped_effects(ability, scope, &mut effects);
    effects
}

fn push_scoped_effects<'a>(
    ability: &'a AbilityDefinition,
    scope: AbilityScope,
    out: &mut Vec<&'a Effect>,
) {
    out.push(&ability.effect);
    if let Some(sub) = &ability.sub_ability {
        push_scoped_effects(sub, scope, out);
    }
    if scope == AbilityScope::Unconditional {
        return;
    }
    // CR 608.2c: the "Otherwise, ..." leg is one of two mutually exclusive
    // outcomes, so it is potential rather than unconditional.
    if let Some(other) = &ability.else_ability {
        push_scoped_effects(other, scope, out);
    }
    // CR 700.2: each mode is an alternative the controller may choose.
    for mode in &ability.mode_abilities {
        push_scoped_effects(mode, scope, out);
    }
}

/// Walk `ability.effect` plus each `sub_ability.effect` in turn, returning
/// borrowed references in chain order. Shorthand for
/// [`collect_scoped_effects`] at [`AbilityScope::Unconditional`].
pub(crate) fn collect_chain_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    collect_scoped_effects(ability, AbilityScope::Unconditional)
}
