//! Combat / damage-marked assertions (CR 120 / CR 510).

use engine::game::scenario::GameRunner;

use super::{AssertionFailure, HandleMap};

pub fn assert_creature_damage(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
    expected: u32,
) -> Result<(), AssertionFailure> {
    let id = handles.get(creature).ok_or_else(|| AssertionFailure {
        kind: "creature_damage".into(),
        detail: format!("unknown creature handle {creature:?}"),
    })?;
    let obj = runner
        .state()
        .objects
        .get(id)
        .ok_or_else(|| AssertionFailure {
            kind: "creature_damage".into(),
            detail: format!("object {id:?} ({creature}) missing"),
        })?;
    if obj.damage_marked != expected {
        return Err(AssertionFailure {
            kind: "creature_damage".into(),
            detail: format!(
                "{creature}: expected damage {expected}, got {}",
                obj.damage_marked
            ),
        });
    }
    Ok(())
}
