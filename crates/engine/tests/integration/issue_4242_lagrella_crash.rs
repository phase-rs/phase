//! Issue #4242: Lagrella, the Magpie — after choosing targets for the ETB
//! exile, the client crashed with `can't access property "some", oe is undefined`.
//!
//! Root cause: once every controller had been used under the "controlled by
//! different players" constraint, the remaining optional multi-target slots had
//! empty `current_legal_targets`. Serde omits empty vecs, so the client read
//! `undefined` and called `.some()` on it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LAGRELLA: &str = "When Lagrella enters, exile any number of other target creatures controlled by different players until Lagrella leaves the battlefield. When an exiled card enters under your control this way, put two +1/+1 counters on it.";

fn gwu_mana() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
    ]
}

#[test]
fn lagrella_etb_exiles_chosen_creatures_after_multi_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, gwu_mana());

    let p0_creature = scenario.add_creature(P0, "P0 Ally", 2, 2).id();
    let p1_creature = scenario.add_creature(P1, "P1 Opp", 2, 2).id();
    let lagrella = scenario
        .add_creature_to_hand_from_oracle(P0, "Lagrella, the Magpie", 2, 2, LAGRELLA)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&lagrella].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: lagrella,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Lagrella");
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Lagrella ETB must pause on trigger target selection"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p1_creature)),
        })
        .expect("choose opponent creature");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p0_creature)),
        })
        .expect("choose ally creature; optional tail auto-completes");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "targeting must not stall on an empty optional slot"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&p1_creature].zone,
        Zone::Exile,
        "opponent creature must be exiled"
    );
    assert_eq!(
        runner.state().objects[&p0_creature].zone,
        Zone::Exile,
        "ally creature must be exiled"
    );
    assert!(
        runner.state().objects[&lagrella].zone == Zone::Battlefield,
        "Lagrella must remain on the battlefield"
    );
}
