//! Forge card script bridge parser.
//!
//! Reads user-supplied Forge card data and selectively replaces `Unimplemented`
//! entries in Oracle-parsed cards. As the Oracle parser improves, cards naturally
//! graduate away from Forge data.
//!
//! Feature-gated behind `forge` cargo feature. Nothing GPL is bundled or
//! distributed; users supply their own Forge checkout.

mod cost;
mod effect;
mod filter;
mod keyword;
pub(crate) mod loader;
mod replacement;
mod static_ab;
mod svar;
mod translate;
mod trigger;
pub(crate) mod types;

pub use loader::ForgeIndex;
pub use translate::apply_forge_fallback;
