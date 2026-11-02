pub mod audit_projection;
pub(crate) mod clause_shell;
pub mod oracle;
pub(crate) mod oracle_attraction;
pub mod oracle_casting;
pub(crate) mod oracle_class;
pub(crate) mod oracle_classifier;
pub mod oracle_condition;
pub mod oracle_cost;
pub(crate) mod oracle_dispatch;
pub mod oracle_effect;
pub mod oracle_ir;
pub mod oracle_keyword;
pub(crate) mod oracle_level;
pub(crate) mod oracle_modal;
pub mod oracle_nom;
pub mod oracle_quantity;
pub mod oracle_replacement;
pub(crate) mod oracle_saga;
pub(crate) mod oracle_separate_piles;
pub mod oracle_spacecraft;
pub(crate) mod oracle_special;
pub mod oracle_static;
pub mod oracle_target;
pub mod oracle_trigger;
pub(crate) mod oracle_un;
pub mod oracle_util;
pub mod oracle_vote;
pub(crate) mod swallow_check;
pub(crate) mod swallow_evidence;
#[cfg(test)]
#[allow(dead_code)] // shared parser test assertions; ad-hoc call sites converted incrementally
pub(crate) mod test_support;

pub use oracle::parse_oracle_text;
pub use oracle::parse_oracle_text_traced;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("missing required field: {0}")]
    MissingField(String),

    #[error("invalid mana cost shard: {0}")]
    InvalidManaCostShard(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
