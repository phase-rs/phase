//! Phase 13 — structural diff infrastructure.
//!
//! This module compares two card-data sources (the native `oracle_nom`
//! parser output and the mtgish-import output) at the structural-JSON
//! level, classifying each divergence by severity. The goal is to surface
//! silent failures in either parser by detecting where they disagree about
//! a card's typed shape.
//!
//! # Pipeline
//! ```text
//!   native_card.json     mtgish_card.json
//!         │                     │
//!         ▼                     ▼
//!     canonicalize          canonicalize       (default-stripping, key-sorting)
//!         │                     │
//!         └──────► classify ◄───┘              (path walk + Severity)
//!                     │
//!                     ▼
//!               Vec<Divergence>
//! ```
//!
//! # Why BTreeMap, not HashMap
//! Every container in the canonical/classify path uses `BTreeMap` for
//! deterministic key ordering. A diff is only meaningful if it is
//! reproducible: two runs over the same inputs MUST produce byte-identical
//! reports. `HashMap` iteration order is randomized per process, which
//! would silently scramble the path walk and emit phantom diffs.

pub mod canonical;
pub mod classify;
pub mod ordering;

pub use self::canonical::canonicalize;
pub use self::classify::{classify_value, Divergence, Severity};
pub use self::ordering::{lookup_ordering, OrderingClass, ORDERING_MANIFEST};
