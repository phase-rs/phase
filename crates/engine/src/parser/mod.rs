pub mod oracle;
pub mod oracle_casting;
pub(crate) mod oracle_class;
pub(crate) mod oracle_classifier;
pub mod oracle_condition;
pub mod oracle_cost;
pub(crate) mod oracle_dispatch;
pub mod oracle_effect;
pub(crate) mod oracle_keyword;
pub(crate) mod oracle_level;
pub(crate) mod oracle_modal;
pub mod oracle_nom;
pub(crate) mod oracle_quantity;
pub mod oracle_replacement;
pub(crate) mod oracle_saga;
pub(crate) mod oracle_spacecraft;
pub(crate) mod oracle_special;
pub mod oracle_static;
pub mod oracle_target;
pub(crate) mod oracle_target_scope;
pub mod oracle_trigger;
pub mod oracle_util;
pub(crate) mod oracle_vote;
pub mod oracle_warnings;

pub use oracle::parse_oracle_text;

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
