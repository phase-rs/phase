//! Integration test for GitHub issue #1332: Bronzehide Lion dies with no legal
//! enchant targets must resolve cleanly (CR 303.4g → graveyard) without hanging.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;

const BRONZEHIDE_LION_ORACLE: &str =
    "{G}{W}: This creature gains indestructible until end of turn.\n\
When this creature dies, return it to the battlefield. It's an Aura enchantment with enchant \
creature you control and \"{G}{W}: Enchanted creature gains indestructible until end of turn,\" \
and it loses all other abilities.";

fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded safety bound; waiting_for = {:?}, stack = {}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("P0 must exist")
        .mana_pool;
    for color in mana {
        pool.add(ManaUnit::new(*color, ObjectId(0), false, vec![]));
    }
}

#[test]
fn bronzehide_lion_dies_with_no_creature_you_control_returns_then_graveyards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    let lion_id = scenario
        .add_creature_from_oracle(P0, "Bronzehide Lion", 3, 3, BRONZEHIDE_LION_ORACLE)
        .id();

    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), lion_id, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);

    assert_eq!(runner.state().stack.len(), 1);

    drain_to_priority(&mut runner);

    let lion = &runner.state().objects[&lion_id];
    assert_eq!(lion.zone, Zone::Graveyard);
    assert!(!lion.base_trigger_definitions.is_empty());
    assert!(runner.state().stack.is_empty());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReturnAsAuraTarget { .. }
    ));
}

/// #5681: Bronzehide's returned Aura grants an activated ability
/// whose indestructible effect expires at end of turn. The trailing comma inside
/// the quoted grant must not erase that duration before the ability reaches the
/// resolver.
#[test]
fn bronzehide_lion_aura_grant_expires_at_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let lion_id = scenario
        .add_creature_from_oracle(P0, "Bronzehide Lion", 3, 3, BRONZEHIDE_LION_ORACLE)
        .id();
    let host_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), lion_id, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&lion_id].attached_to,
        Some(host_id.into()),
        "the returned Aura must attach to the sole legal creature"
    );

    add_mana(&mut runner, &[ManaType::Green, ManaType::White]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: lion_id,
            ability_index: 0,
        })
        .expect("the Aura's granted {G}{W} ability must be activatable");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(host_id)),
            })
            .expect("choose the enchanted creature if the grant surfaces a target slot");
    }
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&host_id]
            .keywords
            .contains(&Keyword::Indestructible),
        "the granted ability must make the enchanted creature indestructible before end of turn"
    );

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&host_id]
            .keywords
            .contains(&Keyword::Indestructible),
        "the granted indestructible must expire at end of turn rather than remain permanent"
    );
}
