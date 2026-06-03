//! Shared parser test helpers.
//!
//! `#[cfg(test)]`-only assertions reused across parser test modules. Provides a
//! single `assert_no_unimplemented` that covers both the `AbilityDefinition`
//! ability tree and the `ParsedEffectClause` effect chain (the two AST shapes
//! that carry a head `Effect` plus a `sub_ability` chain), plus a leaf
//! `assert_unimplemented` for asserting a single effect *is* an
//! `Effect::Unimplemented`.
//!
//! NOTE: This module is purely *additive*. The two existing ad-hoc
//! `assert_no_unimplemented` copies (`oracle_trigger.rs` over
//! `AbilityDefinition`, `oracle_effect/mod.rs` over `ParsedEffectClause`) are
//! intentionally left in place — converting those ~2 call sites (and the ~250
//! other ad-hoc Unimplemented checks across the parser tests) is a later
//! migration pass.

#![cfg(test)]

use crate::parser::oracle_ir::ast::ParsedEffectClause;
use crate::types::ability::{AbilityDefinition, Effect};

/// A node in an effect/ability chain: a head effect plus an optional
/// `sub_ability` continuation. Implemented for both AST shapes so one assertion
/// walks either.
pub(crate) trait EffectChainNode {
    /// The effect carried by this node.
    fn node_effect(&self) -> &Effect;
    /// The next link in the chain, if any.
    fn next_link(&self) -> Option<&AbilityDefinition>;
}

impl EffectChainNode for AbilityDefinition {
    fn node_effect(&self) -> &Effect {
        // `effect` is `Box<Effect>` on AbilityDefinition.
        &self.effect
    }

    fn next_link(&self) -> Option<&AbilityDefinition> {
        self.sub_ability.as_deref()
    }
}

impl EffectChainNode for ParsedEffectClause {
    fn node_effect(&self) -> &Effect {
        &self.effect
    }

    fn next_link(&self) -> Option<&AbilityDefinition> {
        self.sub_ability.as_deref()
    }
}

/// Assert that no link in a parsed chain is `Effect::Unimplemented` — the head
/// node and every `sub_ability` continuation must have parsed to a concrete
/// effect. Works for both `AbilityDefinition` ability trees and
/// `ParsedEffectClause` effect chains via [`EffectChainNode`].
pub(crate) fn assert_no_unimplemented<N: EffectChainNode>(node: &N) {
    assert!(
        !matches!(node.node_effect(), Effect::Unimplemented { .. }),
        "head link is Unimplemented: {:?}",
        node.node_effect()
    );
    let mut cursor = node.next_link();
    while let Some(link) = cursor {
        assert!(
            !matches!(*link.effect, Effect::Unimplemented { .. }),
            "a sub_ability link is Unimplemented: {:?}",
            link.effect
        );
        cursor = link.sub_ability.as_deref();
    }
}

/// Assert that a single effect is an `Effect::Unimplemented` fallback (the
/// parser error boundary, CR-agnostic — used to confirm a pattern is *not yet*
/// supported and routes to the diagnostic fallback).
pub(crate) fn assert_unimplemented(effect: &Effect) {
    assert!(
        matches!(effect, Effect::Unimplemented { .. }),
        "expected Effect::Unimplemented, got {effect:?}"
    );
}
