//! Issue #4780 — Druid of Purification: "When this creature enters, starting
//! with you, each player may choose an artifact or enchantment you don't
//! control. Destroy each permanent chosen this way."
//!
//! Two-layer regression:
//!   1. Parser: "Destroy each permanent chosen this way" lowered to
//!      `DestroyAll { target: Typed(Permanent) }` — a full board wipe — instead
//!      of the published tracked set of chosen permanents.
//!   2. Runtime: `Effect::TargetOnly` (the per-player "choose" step) never
//!      published its chosen objects, so even a correct
//!      `DestroyAll { TrackedSet }` read an empty set and destroyed nothing.
//!
//! Discriminating test: two opponent artifacts on the battlefield; the caster
//! chooses ONE. Only the chosen artifact is destroyed; the un-chosen artifact
//! and the caster's own artifact survive. Fails in both broken states: the
//! board-wipe parse kills all three; the unpublished-set runtime kills none.
//!
//! CR 608.2c: "chosen this way" is an anaphor over the published tracked set.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRUID_ORACLE: &str =
    "When this creature enters, starting with you, each player may choose an \
     artifact or enchantment you don't control. Destroy each permanent chosen this way.";

#[test]
fn druid_of_purification_destroys_only_the_chosen_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let opp_artifact_a = {
        let mut b = scenario.add_creature(P1, "Opp Artifact A", 0, 1);
        b.as_artifact();
        b.id()
    };
    let opp_artifact_b = {
        let mut b = scenario.add_creature(P1, "Opp Artifact B", 0, 1);
        b.as_artifact();
        b.id()
    };
    let own_artifact = {
        let mut b = scenario.add_creature(P0, "Own Artifact", 0, 1);
        b.as_artifact();
        b.id()
    };

    let druid = scenario
        .add_creature_to_hand_from_oracle(P0, "Druid of Purification", 2, 2, DRUID_ORACLE)
        .id();

    let mut runner = scenario.build();
    if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == P0) {
        p.mana_pool.mana = (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect();
    }

    // Cast Druid — its ETB fires and pauses at the per-player choose prompts.
    let card_id = runner.state().objects[&druid].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: druid,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Druid");
    runner.advance_until_stack_empty();

    // Drive the per-player "may choose" flow: P0 picks opponent artifact A
    // (via the trigger's target-selection slot); every other prompt declines.
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: player == P0,
                    })
                    .expect("optional choice decision");
            }
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::ChooseObjectsSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(opp_artifact_a)],
                    })
                    .expect("select opponent artifact A");
            }
            _ => break,
        }
        runner.advance_until_stack_empty();
    }

    assert_eq!(
        runner.state().objects.get(&opp_artifact_a).map(|o| o.zone),
        Some(Zone::Graveyard),
        "the chosen opponent artifact must be destroyed"
    );
    assert_eq!(
        runner.state().objects.get(&opp_artifact_b).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the un-chosen opponent artifact must survive (not a board wipe)"
    );
    assert_eq!(
        runner.state().objects.get(&own_artifact).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the caster's own artifact must survive"
    );
}
