//! Regression for issue #1319: Ninjutsu/Sneak must not target attackers that
//! left the battlefield before or during the declare blockers step.
//!
//! https://github.com/phase-rs/phase/issues/1319

use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{Keyword, ObjectId};

#[test]
fn issue_1319_ninjutsu_rejects_attacker_that_left_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);

    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let ninja = scenario.add_creature_to_hand(P0, "Ninja", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    {
        let obj = runner.state_mut().objects.get_mut(&ninja).unwrap();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        };
        obj.keywords.push(Keyword::Ninjutsu(cost.clone()));
        obj.base_keywords.push(Keyword::Ninjutsu(cost));
    }

    {
        let state = runner.state_mut();
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.priority_player = P0;
    }

    runner
        .act(GameAction::Debug(DebugAction::MoveToZone {
            object_id: attacker,
            to_zone: Zone::Graveyard,
            library_position: None,
            simulate: false,
        }))
        .expect("destroy attacker before ninjutsu");

    let err = runner
        .act(GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: attacker,
        })
        .expect_err("ninjutsu on dead attacker must fail");
    assert!(
        err.to_string().contains("no longer on the battlefield")
            || err.to_string().contains("not an attacker"),
        "unexpected error: {err}"
    );
}
