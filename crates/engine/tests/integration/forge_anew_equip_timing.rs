use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const FORGE_ANEW: &str = "When this enchantment enters, return target Equipment card from your graveyard to the battlefield.\nDuring your turn, you may activate equip abilities any time you could cast an instant.\nYou may pay {0} rather than pay the equip cost of the first equip ability you activate during each of your turns.";
const LEONIN_SHIKARI: &str = "You may activate equip abilities any time you could cast an instant.";
const VULSHOK_MORNINGSTAR: &str = "Equipped creature gets +2/+2.\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";

fn equip_scenario(mut scenario: GameScenario, active: PlayerId) -> (GameRunner, ObjectId) {
    scenario.at_phase(Phase::Upkeep);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = active;
    (runner, equipment)
}

fn activate_equip(equipment: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: equipment,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }
}

// CR 101.1 + CR 109.5 + CR 117.1a + CR 702.6a: priority in either upkeep
// allows an instant, but Forge Anew lifts equip's sorcery timing only on its
// controller's turn. Keep the own-turn reach guard in the same test.
#[test]
fn forge_anew_allows_equip_in_your_upkeep_but_rejects_it_in_the_opponents_upkeep() {
    let setup = |active| {
        let mut scenario = GameScenario::new();
        scenario.add_enchantment_from_oracle(P0, "Forge Anew", FORGE_ANEW);
        let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let (runner, equipment) = equip_scenario(scenario, active);
        (runner, equipment, creature)
    };

    let (mut own_turn, equipment, creature) = setup(P0);
    let activation = own_turn
        .act(activate_equip(equipment))
        .expect("Forge Anew allows equip during its controller's upkeep");
    let WaitingFor::ActivationCostOneOfChoice { player, costs, .. } = activation.waiting_for else {
        panic!("expected the ordinary and alternative equip costs");
    };
    assert_eq!(player, P0);
    let ordinary = costs
        .iter()
        .position(
            |cost| matches!(cost, AbilityCost::Mana { cost } if cost == &ManaCost::generic(2)),
        )
        .expect("the ordinary equip cost should be available");
    own_turn
        .act(GameAction::ChooseActivationCostBranch { index: ordinary })
        .expect("the ordinary equip cost should be payable");
    assert_eq!(own_turn.state().stack.len(), 1);
    own_turn.advance_until_stack_empty();
    assert!(matches!(
        own_turn.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(own_turn.state().objects[&creature]
        .attachments
        .contains(&equipment));
    assert_eq!(own_turn.state().players[0].mana_pool.total(), 0);

    let (mut opponents_turn, equipment, creature) = setup(P1);
    let error = opponents_turn
        .act(activate_equip(equipment))
        .expect_err("Forge Anew cannot lift equip timing in the opponent's upkeep");
    assert!(
        matches!(error, EngineError::ActionNotAllowed(ref reason) if reason == "Activation restriction not satisfied: AsSorcery"),
        "{error:?}"
    );
    assert!(opponents_turn.state().stack.is_empty());
    assert!(!opponents_turn.state().objects[&creature]
        .attachments
        .contains(&equipment));
    assert_eq!(opponents_turn.state().players[0].mana_pool.total(), 2);
}

#[test]
fn leonin_shikari_allows_equip_in_the_opponents_upkeep() {
    let mut scenario = GameScenario::new();
    let shikari = scenario
        .add_creature_from_oracle(P0, "Leonin Shikari", 2, 2, LEONIN_SHIKARI)
        .with_subtypes(vec!["Cat", "Soldier"])
        .id();
    let (mut runner, equipment) = equip_scenario(scenario, P1);

    runner
        .act(activate_equip(equipment))
        .expect("Leonin Shikari allows equip during the opponent's upkeep");
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().objects[&shikari]
        .attachments
        .contains(&equipment));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}
