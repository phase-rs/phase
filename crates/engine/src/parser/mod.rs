pub mod ability;
pub mod card_parser;
pub mod card_type;
pub mod mana_cost;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("missing required field: {0}")]
    MissingField(String),

    #[error("missing ability kind (SP$/AB$/DB$)")]
    MissingAbilityKind,

    #[error("invalid mana cost shard: {0}")]
    InvalidManaCostShard(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
