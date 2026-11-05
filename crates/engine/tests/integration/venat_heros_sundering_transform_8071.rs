//! Regression for GitHub issue #8071 — Venat, Heart of Hydaelyn.
//!
//! "Hero's Sundering — {7}, {T}: Exile target nonland permanent. Transform
//! Venat. Activate only as a sorcery."
//!
//! CR 701.27a: Transform turns a double-faced permanent to its other face.
//! CR 608.2c: the instructions resolve in written order, so the exile does not
//! stop the later "Transform Venat".

use crate::support::shared_card_db;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn heros_sundering_exiles_the_target_and_transforms_venat() {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let venat = scenario.add_real_card(P0, "Venat, Heart of Hydaelyn", Zone::Battlefield, db);
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        for _ in 0..7 {
            state
                .players
                .iter_mut()
                .find(|p| p.id == P0)
                .unwrap()
                .mana_pool
                .add(ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        state.objects.get_mut(&venat).unwrap().summoning_sick = false;
    }
    assert_eq!(
        runner.state().objects[&venat].name,
        "Venat, Heart of Hydaelyn"
    );
    assert!(!runner.state().objects[&venat].transformed);

    let ability_index = 0;
    runner
        .act(GameAction::ActivateAbility {
            source_id: venat,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Hero's Sundering activates");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(victim)],
        })
        .expect("the target is legal");
    assert!(
        !runner.state().stack.is_empty(),
        "the ability is on the stack (positive reach guard)"
    );
    runner.resolve_top();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "the target is exiled"
    );
    let v = &runner.state().objects[&venat];
    assert_eq!(v.zone, Zone::Battlefield);
    assert!(v.transformed, "Venat transformed");
    assert_eq!(v.name, "Hydaelyn, the Mothercrystal");
}
