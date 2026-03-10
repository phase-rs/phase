// Integration test entry point for rules correctness tests.
// Common imports re-exported for all rule test modules via `use super::*`.
#![allow(unused_imports)]

pub use engine::game::apply;
pub use engine::game::scenario::{GameScenario, P0, P1};
pub use engine::types::actions::GameAction;
pub use engine::types::events::GameEvent;
pub use engine::types::game_state::{ActionResult, WaitingFor};
pub use engine::types::keywords::Keyword;
pub use engine::types::phase::Phase;
pub use engine::types::player::PlayerId;
pub use engine::types::zones::Zone;

// Mechanic test modules (stubs -- populated in Plans 02 and 03)
#[path = "rules/combat.rs"]
mod combat;
#[path = "rules/etb.rs"]
mod etb;
#[path = "rules/keywords.rs"]
mod keywords;
#[path = "rules/layers.rs"]
mod layers;
#[path = "rules/sba.rs"]
mod sba;
#[path = "rules/stack.rs"]
mod stack;
#[path = "rules/targeting.rs"]
mod targeting;
