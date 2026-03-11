pub mod ability;
pub mod oracle_util;
pub mod oracle_target;
pub mod oracle_cost;
pub mod oracle_effect;
pub mod oracle_trigger;
pub mod oracle_static;
pub mod oracle_replacement;
#[cfg(feature = "forge-compat")]
pub mod card_parser;
#[cfg(feature = "forge-compat")]
pub mod card_type;
#[cfg(feature = "forge-compat")]
pub mod mana_cost;

#[cfg(feature = "forge-compat")]
pub use card_parser::parse_card_file;

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
