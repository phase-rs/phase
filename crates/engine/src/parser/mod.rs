pub mod oracle;
pub mod oracle_cost;
pub mod oracle_effect;
pub mod oracle_replacement;
pub mod oracle_static;
pub mod oracle_target;
pub mod oracle_trigger;
pub mod oracle_util;

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
