//! Executable Comprehensive Rules scenario suite.
//!
//! Turns CR annotations from documentation into enforceable contracts:
//! CompRules → fixture skeletons → typed assertions executed via `GameScenario`.
//!
//! See issue #6343 and `README.md` in this crate.

pub mod assert;
pub mod catalog;
pub mod comp_rules;
pub mod generate;
pub mod loader;
pub mod predicates;
pub mod report;
pub mod runner;
pub mod schema;
pub mod section_plans;
pub mod steps_catalog;

pub use catalog::{is_included_section, section_title, INCLUDED_SECTIONS};
pub use generate::{generate_skeletons, GenerateOptions, GenerateStats};
pub use loader::{load_scenario_file, load_scenarios, ScenarioLoadError};
pub use report::{ScenarioReport, SuiteReport};
pub use runner::{run_scenario, run_suite, RunFilter, RunOptions, ScenarioOutcome};
pub use schema::{
    AssertionSpec, CreatureSpec, LightningBoltSpec, PlayerSetup, ScenarioFile, ScenarioStatus,
    ScenarioStep, SetupSpec,
};
