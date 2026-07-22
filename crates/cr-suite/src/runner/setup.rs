//! Build a `GameRunner` from a declarative setup.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::assert::{parse_phase, HandleMap};
use crate::runner::{RunError, ScenarioContext};
use crate::schema::{CreatureSpec, SetupSpec};

pub fn build_runner(setup: &SetupSpec) -> Result<(GameRunner, ScenarioContext), RunError> {
    let mut scenario = if let Some(seed) = setup.seed {
        GameScenario::new_n_player(2, seed)
    } else {
        GameScenario::new()
    };

    let phase = parse_phase(&setup.phase).map_err(|e| RunError::Setup(e.detail))?;
    scenario.at_phase(phase);

    for player in &setup.players {
        let pid = PlayerId(player.id);
        scenario.with_life(pid, player.life);
        if !player.hand.is_empty() {
            let names: Vec<&str> = player.hand.iter().map(|s| s.as_str()).collect();
            scenario.with_cards_in_hand(pid, &names);
        }
        if !player.library_top.is_empty() {
            let names: Vec<&str> = player.library_top.iter().map(|s| s.as_str()).collect();
            scenario.with_library_top(pid, &names);
        }
    }

    let mut handles = HandleMap::new();
    for creature in &setup.creatures {
        let id = place_creature(&mut scenario, creature)?;
        if handles.insert(creature.id.clone(), id).is_some() {
            return Err(RunError::Setup(format!(
                "duplicate creature handle {:?}",
                creature.id
            )));
        }
    }

    Ok((scenario.build(), ScenarioContext { handles }))
}

fn place_creature(
    scenario: &mut GameScenario,
    creature: &CreatureSpec,
) -> Result<engine::types::identifiers::ObjectId, RunError> {
    let pid = PlayerId(creature.player);
    let mut builder =
        scenario.add_creature(pid, &creature.name, creature.power, creature.toughness);

    for kw_name in &creature.keywords {
        let kw: Keyword = kw_name
            .parse()
            .unwrap_or_else(|_| Keyword::Unknown(kw_name.clone()));
        if matches!(kw, Keyword::Unknown(_)) {
            return Err(RunError::Setup(format!(
                "unknown keyword {kw_name:?} on creature {:?}",
                creature.id
            )));
        }
        builder.with_keyword(kw);
    }

    if creature.damage_marked > 0 {
        builder.with_damage_marked(creature.damage_marked);
    }
    if creature.summoning_sickness {
        builder.with_summoning_sickness();
    }

    Ok(builder.id())
}
