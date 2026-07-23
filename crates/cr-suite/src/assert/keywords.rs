//! Keyword-presence assertions (CR 702).
//!
//! Reads the engine's post-layer keyword list via
//! [`engine::game::keywords::has_keyword`], which is authoritative for
//! battlefield objects (CR 613 layer system already applied). This is a
//! state-read only — no keyword semantics are re-derived here.

use engine::game::keywords::has_keyword;
use engine::game::scenario::GameRunner;
use engine::types::keywords::Keyword;

use super::{AssertionFailure, HandleMap};

/// Assert a named battlefield creature has the given keyword (CR 702).
///
/// `keyword_name` is parsed through `Keyword::from_str`; unknown names fail
/// loudly rather than being silently treated as absent.
pub fn assert_creature_has_keyword(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
    keyword_name: &str,
) -> Result<(), AssertionFailure> {
    let keyword: Keyword = keyword_name.parse().map_err(|_| AssertionFailure {
        kind: "creature_keyword".into(),
        detail: format!("unknown keyword name {keyword_name:?}"),
    })?;
    let id = handles.get(creature).ok_or_else(|| AssertionFailure {
        kind: "creature_keyword".into(),
        detail: format!("unknown creature handle {creature:?}"),
    })?;
    let obj = runner
        .state()
        .objects
        .get(id)
        .ok_or_else(|| AssertionFailure {
            kind: "creature_keyword".into(),
            detail: format!("object {id:?} ({creature}) missing"),
        })?;
    if has_keyword(obj, &keyword) {
        Ok(())
    } else {
        Err(AssertionFailure {
            kind: "creature_keyword".into(),
            detail: format!("{creature} does not have keyword {keyword_name}"),
        })
    }
}
