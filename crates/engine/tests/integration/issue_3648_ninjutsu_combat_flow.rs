//! Regression for GitHub issue #3648 — Ninjutsu offered during real combat flow.
//!
//! Mana pools empty when leaving the main phase (CR 500.4), so "open mana" at
//! combat means untapped mana sources the player can still tap during the
//! declare blockers step.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::NinjutsuVariant;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;
use crate::support::shared_card_db as load_db;
use engine::game::scenario_db::GameScenarioDbExt;

#[test]
fn ninjutsu_offered_after_declare_attackers_with_tappable_mana_sources() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let hand_ninja = scenario.add_creature_to_hand(P0, "Hand Ninja", 2, 2).id();
    let _island1 = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let _island2 = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&hand_ninja).unwrap();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        obj.keywords.push(Keyword::Ninjutsu(cost.clone()));
        obj.base_keywords.push(Keyword::Ninjutsu(cost));
    }

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");

    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }

    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
    }

    assert_eq!(runner.state().phase, Phase::DeclareBlockers);
    match &runner.state().waiting_for {
        WaitingFor::Priority { player } => {
            assert_eq!(*player, P0);
            assert!(
                engine::ai_support::legal_actions(runner.state())
                    .iter()
                    .any(|a| {
                        matches!(
                            a,
                            GameAction::ActivateNinjutsu {
                                ninjutsu_object_id,
                                creature_to_return,
                            } if *ninjutsu_object_id == hand_ninja
                                && *creature_to_return == attacker
                        )
                    }),
                "ninjutsu must be offered when untapped sources can pay during declare blockers"
            );
        }
        other => panic!("expected Priority for P0 after blockers, got {other:?}"),
    }
}

#[test]
fn ninjutsu_family_returnable_includes_unblocked_attacker_after_real_combat_setup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
    }

    let returnable = engine::game::keywords::returnable_creatures_for_variant(
        runner.state(),
        P0,
        &NinjutsuVariant::Ninjutsu,
    );
    assert_eq!(returnable, vec![attacker]);
}
