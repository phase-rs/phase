//! Vendored copy of the mtgish hand-crafted MTG type schema.
//!
//! Source: `mtgish/rust_syntax/src/mtg_types.rs` (commit-pinned via the
//! `mtgish/` directory in this repo).
//!
//! Vendored verbatim with three mechanical edits performed by the import
//! script in `crates/mtgish-import/README.md`:
//! 1. Strip `ts_rs::TS` derives and `#[ts(export)]` attributes.
//! 2. Strip `bincode::{Encode,Decode}` derives.
//! 3. Promote `#[cfg_attr(feature = "write_out_json", serde(...))]` to
//!    unconditional `#[serde(...)]` — the `__build/cards.json` file was
//!    emitted with that feature on, so the same tagging is required to
//!    deserialize it.

// Vendored AST — `large_enum_variant` is intrinsic to mtgish's schema (300+
// variants on `Action`, `Permanents`, `GameNumber`, etc.); Box-ing in the
// vendored copy would require maintaining a structural diff against upstream.
// The runtime cost is acceptable for a build-time import binary.
#[allow(clippy::large_enum_variant)]
pub mod types;
