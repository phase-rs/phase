//! Issue #3299 — Syr Konrad must deal 1 damage to each opponent per trigger event,
//! not doubled because the disjunctive zone-change condition was compound-split
//! into separate triggers.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const KONRAD_ORACLE: &str = "Whenever another creature dies, or a creature card \
    is put into a graveyard from anywhere other than the battlefield, or a \
    creature card leaves your graveyard, Syr Konrad, the Grim deals 1 damage \
    to each opponent.\n{1}{B}: Each player mills a card.";

fn drain_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
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

fn stack_library(
    state: &mut engine::types::game_state::GameState,
    player: PlayerId,
    core_types: &[CoreType],
) {
    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
        let existing: Vec<_> = p.library.iter().copied().collect();
        for id in &existing {
            state.objects.remove(id);
        }
        p.library.clear();
    }
    for (i, &core_type) in core_types.iter().enumerate() {
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let id = engine::game::zones::create_object(
            state,
            card_id,
            player,
            format!("Library Card {i}"),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core_type);
        obj.base_card_types = obj.card_types.clone();
    }
}

#[test]
fn syr_konrad_deals_one_damage_when_opponent_creature_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Syr Konrad, the Grim", 5, 4, KONRAD_ORACLE)
        .id();
    let fodder = scenario.add_creature(P1, "Fodder", 1, 1).id();

    let mut runner = scenario.build();
    let p1_start_life = runner.state().players[1].life;

    runner
        .state_mut()
        .objects
        .get_mut(&fodder)
        .unwrap()
        .damage_marked = 2;

    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);
    engine::game::triggers::process_triggers(runner.state_mut(), &sba_events);
    drain_stack(&mut runner);

    assert_eq!(
        p1_start_life - runner.state().players[1].life,
        1,
        "Konrad should deal exactly 1 damage when one opponent creature dies"
    );
}

#[test]
fn syr_konrad_deals_one_damage_per_milled_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let konrad = scenario
        .add_creature_from_oracle(P0, "Syr Konrad, the Grim", 5, 4, KONRAD_ORACLE)
        .id();

    let mut runner = scenario.build();
    stack_library(runner.state_mut(), P1, &[CoreType::Creature]);

    let p1_start_life = runner.state().players[1].life;
    let dummy = ObjectId(0);
    {
        let p0 = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap();
        p0.mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: dummy,
            pip_id: engine::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
        p0.mana_pool.add(ManaUnit {
            color: ManaType::Black,
            source_id: dummy,
            pip_id: engine::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: konrad,
            ability_index: 0,
        })
        .expect("activate mill");
    drain_stack(&mut runner);

    assert_eq!(
        p1_start_life - runner.state().players[1].life,
        1,
        "one milled creature should deal exactly 1 damage, not doubled"
    );
}
