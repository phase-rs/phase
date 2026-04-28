//! mtgish → phase.rs engine import.
//!
//! Loads `data/mtgish-cards.json` (a snapshot of the JSON produced by the
//! mtgish Go parser) using the vendored mtgish type schema, and converts
//! each card into the phase.rs engine's runtime card-data shape. Output
//! drops directly into `data/card-data.json` consumers (the WASM bridge,
//! the server, the AI), so the engine never knows the abilities came
//! from a different parser.
//!
//! # Pipeline
//! ```text
//!   data/mtgish-cards.json
//!     │  serde_json (with vendored schema/types.rs)
//!     ▼
//!   schema::OracleCard
//!     │  convert::card
//!     ▼
//!   engine::types::card::CardFace + abilities/triggers/statics/replacements
//!     │  serde_json
//!     ▼
//!   data/card-data.mtgish.json   (compatible with engine's CardDatabase::from_export)
//! ```

pub mod convert;
pub mod diff;
pub mod provenance;
pub mod report;
pub mod schema;
