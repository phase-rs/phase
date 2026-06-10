//! Standardized result type for Oracle text parser combinators.
//!
//! Provides the shared `OracleResult` type alias and `oracle_err` error
//! constructor used across all nom-based parser branches.
//!
//! For expressing "the parser couldn't handle this", the single authority is
//! `Effect::unimplemented(name, fragment)` (see `types/ability.rs`) — parser
//! code must never hand-construct `Effect::Unimplemented { .. }` literals
//! (enforced for new code by `scripts/check-parser-combinators.sh`).

use nom::IResult;

pub type OracleError<'a> = nom::error::Error<&'a str>;

pub type OracleResult<'a, O> = IResult<&'a str, O, OracleError<'a>>;

pub fn oracle_err(input: &str) -> nom::Err<OracleError<'_>> {
    nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
}
