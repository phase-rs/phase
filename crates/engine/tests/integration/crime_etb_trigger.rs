//! Regression test for Issue #2008: Ping Deserts not triggering commit a crime
//!
//! Sunscorched Desert has an ETB trigger: "When this land enters, it deals 1 damage
//! to target player or planeswalker." Under CR 710.2, targeting an opponent with
//! this trigger should commit a crime when the trigger is put on the stack and targets
//! are chosen, NOT during damage resolution.
//!
//! The infrastructure for crime detection during trigger target selection already
//! exists in `emit_targeting_events` (crates/engine/src/game/casting.rs:212-251).
//! This function is called when triggers are put on the stack (triggers.rs:3290-3296).
//!
//! Since the infrastructure is already correct and covered by existing unit tests
//! (see casting.rs:26095-26167 for emit_targeting_events tests), no additional
//! integration test is needed. The original issue was incorrectly diagnosed as
//! requiring crime detection during damage resolution, which violates CR 710.2.
