pub mod ai_support;
pub mod database;
pub mod game;
pub mod parser;
pub mod starter_decks;
pub mod testing;
pub mod types;
pub mod util;

// Re-export `im` so downstream crates can construct persistent containers
// without declaring their own dependency. Keeps the backing-container choice
// (im vs rpds vs dashmap) centralized here.
pub use im;
