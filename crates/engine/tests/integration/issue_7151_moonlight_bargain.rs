//! Production-path regression for Moonlight Bargain's per-card payment loop.
//!
//! The post-Dig child is repeated once for each of the looked-at cards. Its
//! iteration universe must be the exact five cards supplied by Dig, rather than
//! an unqualified battlefield object census (CR 608.2c, CR 701.20e).

use std::collections::HashSet;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MOONLIGHT_BARGAIN: &str = "Look at the top five cards of your library. For each card, put that card into your graveyard unless you pay 2 life. Then put the rest into your hand.";

fn pending_member(
    runner: &mut GameRunner,
    looked_members: &[engine::types::identifiers::ObjectId],
) -> engine::types::identifiers::ObjectId {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::UnlessPayment {
                player,
                pending_effect,
                ..
            } => {
                assert_eq!(player, P0, "Moonlight Bargain's controller pays life");
                assert_eq!(
                    pending_effect
                        .context
                        .parent_target_iteration_members
                        .as_deref(),
                    Some(looked_members),
                    "each payment prompt retains Dig's exact five-card universe"
                );
                let members: Vec<_> = pending_effect
                    .targets
                    .iter()
                    .filter_map(|target| match target {
                        TargetRef::Object(id) => Some(*id),
                        TargetRef::Player(_) => None,
                    })
                    .collect();
                assert_eq!(
                    members.len(),
                    1,
                    "each iteration has exactly one card target"
                );
                return members[0];
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance Moonlight Bargain");
            }
            other => panic!("expected Moonlight Bargain payment prompt, got {other:?}"),
        }
    }
    panic!("Moonlight Bargain never reached its next payment prompt");
}

#[test]
fn moonlight_bargain_repeats_only_over_the_cards_it_looked_at() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    let moonlight = scenario
        .add_spell_to_hand_from_oracle(P0, "Moonlight Bargain", true, MOONLIGHT_BARGAIN)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(
        P0,
        &["Looked A", "Looked B", "Looked C", "Looked D", "Looked E"],
    );
    let battlefield_a = scenario.add_creature(P0, "Unrelated A", 2, 2).id();
    let battlefield_b = scenario.add_creature(P0, "Unrelated B", 3, 3).id();

    let mut runner = scenario.build();
    runner.cast(moonlight).resolve();
    let looked_members = runner.state().last_revealed_ids.clone();
    assert_eq!(looked_members.len(), 5, "Dig looked at exactly five cards");

    let mut paid_members = Vec::new();
    let mut declined_members = Vec::new();
    for pay in [true, false, true, false, false] {
        let member = pending_member(&mut runner, &looked_members);
        if pay {
            paid_members.push(member);
        } else {
            declined_members.push(member);
        }
        runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("each Moonlight Bargain payment decision succeeds");
    }

    let looked: HashSet<_> = looked_members.into_iter().collect();
    assert_eq!(looked.len(), 5, "Dig looked at five distinct cards");
    assert_eq!(
        paid_members.iter().copied().collect::<HashSet<_>>().len(),
        2,
        "each paid iteration is a distinct looked-at card"
    );
    assert_eq!(
        declined_members
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len(),
        3,
        "each declined iteration is a distinct looked-at card"
    );
    assert!(
        paid_members
            .iter()
            .chain(&declined_members)
            .all(|id| looked.contains(id)),
        "no repeat iteration may substitute an unrelated battlefield permanent"
    );
    assert_eq!(
        runner.state().objects[&battlefield_a].zone,
        Zone::Battlefield
    );
    assert_eq!(
        runner.state().objects[&battlefield_b].zone,
        Zone::Battlefield
    );
}
