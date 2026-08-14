pub mod ai_support;
pub mod analysis;
pub mod database;
pub mod game;
pub mod parser;
pub mod starter_decks;
pub mod testing;
pub mod types;
pub mod util;

#[cfg(test)]
mod test_support;

// The shared comment rule for every source census in this repository. Also compiled into the
// integration binary through a `#[path]` declaration in `tests/integration/main.rs`, so the two
// venues share ONE implementation rather than `test_support.rs`'s twin-sync PAIR.
#[cfg(test)]
mod source_census;

// Re-export `im` so downstream crates can construct persistent containers
// without declaring their own dependency. Keeps the backing-container choice
// (im vs rpds vs dashmap) centralized here.
pub use im;
