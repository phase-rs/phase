//! Re-export shim. The scope-parameterized walker now lives in the engine so
//! `engine::analysis::deck_signals` can share it; every `crate::ability_chain::*`
//! call site in this crate is unchanged.
pub(crate) use engine::analysis::deck_signals::ability_chain::{
    collect_chain_effects, collect_scoped_effects, AbilityScope,
};
