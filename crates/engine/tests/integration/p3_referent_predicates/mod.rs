//! P3 referent-predicate unit. `scope_fixtures` drives the `TargetMatchesFilter` evaluator with
//! hand-built conditions (S1a). The other files are production-path witnesses (S0/S2a) for the
//! mana-value referent gates ("that spell's mana value was N or less", "if that spell has mana
//! value N or greater") and preservation controls over the supported cards that already use the
//! same evaluator. Every spell under test is a real printed card with its verbatim Oracle text
//! (see `oracle.rs`); targets and blockers that carry no text are textless filler bodies.

mod consumers;
mod controls;
mod decl;
mod event;
mod fixture;
mod oracle;
mod scope_fixtures;
