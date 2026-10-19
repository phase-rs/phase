//! Ordinary public spell ownership with genuine host-cost and regeneration controls.

use super::jev_token_followup::{assert_created, board, ORDINARY_CONSTRUCTION};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, ShieldKind};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

#[test]
fn construction_no_regeneration_rider_reaches_destroy() {
    // Explicitly synthetic control differs only by omitting the rider.
    let regenerable = "Destroy target nonartifact attacking creature. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness.";
    for (oracle, expected_zone) in [
        (ORDINARY_CONSTRUCTION, Zone::Graveyard),
        (regenerable, Zone::Battlefield),
    ] {
        let mut b = board(
            oracle,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        b.runner.cast(b.regen).target_object(b.b).resolve();
        // CR 117.3b + CR 117.3d: finish preparation before P0 casts again.
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        b.runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert!(b.runner.state().objects[&b.b]
            .replacement_definitions
            .iter_unchecked()
            .any(|r| r.shield_kind == ShieldKind::Regeneration));
        assert!(b
            .runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|a| a.object_id == b.b));
        let outcome = b.runner.cast(b.spell).target_object(b.b).resolve();
        // CR 701.19a/c + CR 608.2c: the rider suppresses the shield, not creation.
        outcome.assert_zone(&[b.b], expected_zone);
        assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
        if expected_zone == Zone::Battlefield {
            assert!(outcome.state().objects[&b.b].tapped);
            assert!(outcome
                .state()
                .combat
                .as_ref()
                .is_none_or(|c| c.attackers.iter().all(|a| a.object_id != b.b)));
        }
    }
}

#[test]
fn ordinary_host_cost_and_token_definition_keep_distinct_owners() {
    for reduced in [false, true] {
        let text = if reduced {
            format!("This spell costs {{1}} less to cast.\n{ORDINARY_CONSTRUCTION}")
        } else {
            ORDINARY_CONSTRUCTION.into()
        };
        // SHAPE accompanies actual payment and resolution; it is not the runtime proof.
        let parsed = parse_oracle_text(
            &text,
            "Synthetic self-cost Spirit",
            &[],
            &["Instant".into()],
            &[],
        );
        assert_eq!(parsed.statics.len(), usize::from(reduced));
        if reduced {
            assert!(
                matches!(&parsed.statics[0].mode, StaticMode::ModifyCost { mode: CostModifyMode::Reduce, amount, .. } if amount == &ManaCost::generic(1))
            );
        }
        assert_eq!(parsed.abilities.len(), 1);
        let chain: Vec<_> =
            std::iter::successors(Some(&parsed.abilities[0]), |d| d.sub_ability.as_deref())
                .collect();
        assert!(chain
            .iter()
            .all(|d| !matches!(*d.effect, Effect::Unimplemented { .. })));
        assert!(chain
            .iter()
            .any(|d| matches!(*d.effect, Effect::Token { .. })));
        assert_eq!(
            chain
                .iter()
                .filter(|d| matches!(*d.effect, Effect::Destroy { .. }))
                .count(),
            1
        );
        let paid = if reduced { 1 } else { 2 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario
            .add_creature(P1, "Opponent's ordinary B", 7, 8)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic self-cost Spirit", true, &text)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(victim, AttackTarget::Player(P0))])
            .unwrap();
        runner.advance_to_priority_window();
        if runner.state().priority_player == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        for _ in 0..paid {
            runner
                .state_mut()
                .add_mana_to_pool(
                    P0,
                    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                )
                .unwrap();
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), paid);
        let committed = runner.cast(spell).target_object(victim).commit();
        // CR 601.2a/f: one available mana pays printed {2} only with the host reduction.
        assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[victim], Zone::Graveyard);
        assert_created(outcome.state(), outcome.events(), spell, 1, (7, 8));
    }
}
