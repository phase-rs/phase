//! Zone membership assertions (CR 400+).

use engine::game::scenario::GameRunner;
use engine::types::zones::Zone;

use super::{AssertionFailure, HandleMap};

pub fn parse_zone(name: &str) -> Result<Zone, AssertionFailure> {
    match name {
        "Battlefield" => Ok(Zone::Battlefield),
        "Graveyard" => Ok(Zone::Graveyard),
        "Exile" => Ok(Zone::Exile),
        "Hand" => Ok(Zone::Hand),
        "Library" => Ok(Zone::Library),
        "Stack" => Ok(Zone::Stack),
        "Command" => Ok(Zone::Command),
        other => Err(AssertionFailure {
            kind: "creature_zone".into(),
            detail: format!("unknown zone name {other:?}"),
        }),
    }
}

pub fn assert_creature_zone(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
    zone_name: &str,
) -> Result<(), AssertionFailure> {
    let expected = parse_zone(zone_name)?;
    let id = handles.get(creature).ok_or_else(|| AssertionFailure {
        kind: "creature_zone".into(),
        detail: format!("unknown creature handle {creature:?}"),
    })?;
    let obj = runner
        .state()
        .objects
        .get(id)
        .ok_or_else(|| AssertionFailure {
            kind: "creature_zone".into(),
            detail: format!("object {id:?} ({creature}) missing from state"),
        })?;
    if obj.zone != expected {
        return Err(AssertionFailure {
            kind: "creature_zone".into(),
            detail: format!("{creature}: expected zone {expected:?}, got {:?}", obj.zone),
        });
    }
    Ok(())
}

pub fn assert_creature_on_battlefield(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
) -> Result<(), AssertionFailure> {
    assert_creature_zone(runner, handles, creature, "Battlefield")
}

pub fn assert_creature_in_graveyard(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
) -> Result<(), AssertionFailure> {
    assert_creature_zone(runner, handles, creature, "Graveyard")
}
