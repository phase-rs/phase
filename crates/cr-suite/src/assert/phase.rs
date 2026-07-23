//! Phase assertions (CR 500+).

use engine::game::scenario::GameRunner;
use engine::types::phase::Phase;

use super::AssertionFailure;

pub fn parse_phase(name: &str) -> Result<Phase, AssertionFailure> {
    match name {
        "Untap" => Ok(Phase::Untap),
        "Upkeep" => Ok(Phase::Upkeep),
        "Draw" => Ok(Phase::Draw),
        "PreCombatMain" => Ok(Phase::PreCombatMain),
        "BeginCombat" => Ok(Phase::BeginCombat),
        "DeclareAttackers" => Ok(Phase::DeclareAttackers),
        "DeclareBlockers" => Ok(Phase::DeclareBlockers),
        "CombatDamage" => Ok(Phase::CombatDamage),
        "EndCombat" => Ok(Phase::EndCombat),
        "PostCombatMain" => Ok(Phase::PostCombatMain),
        "End" => Ok(Phase::End),
        "Cleanup" => Ok(Phase::Cleanup),
        other => Err(AssertionFailure {
            kind: "phase_is".into(),
            detail: format!("unknown phase name {other:?}"),
        }),
    }
}

pub fn assert_phase_is(runner: &GameRunner, phase_name: &str) -> Result<(), AssertionFailure> {
    let expected = parse_phase(phase_name)?;
    let actual = runner.state().phase;
    if actual != expected {
        return Err(AssertionFailure {
            kind: "phase_is".into(),
            detail: format!("expected phase {expected:?}, got {actual:?}"),
        });
    }
    Ok(())
}
