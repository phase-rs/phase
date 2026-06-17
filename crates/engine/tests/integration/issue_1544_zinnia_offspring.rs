//! Issue #1544: Zinnia, Valley's Voice — granted offspring on cast creature spells.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;

const ZINNIA_ORACLE: &str = "\
Flying\n\
Zinnia gets +X/+0, where X is the number of other creatures you control with base power 1.\n\
Creature spells you cast gain offspring {2} as you cast them.";

fn fund_mana(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    let p0 = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap();
    for _ in 0..count {
        p0.mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }
}

#[test]
fn zinnia_grants_offspring_on_creature_spells_you_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_creature_from_oracle(P0, "Zinnia, Valley's Voice", 1, 1, ZINNIA_ORACLE);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(engine::types::mana::ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;
    fund_mana(&mut runner, 6);

    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast creature with Zinnia in play");

    let mut paid_offspring = false;
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("pay offspring");
                paid_offspring = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            other => panic!("unexpected waiting_for during cast: {other:?}"),
        }
    }
    assert!(paid_offspring, "Zinnia must offer offspring optional cost");

    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        runner.act(GameAction::PassPriority).ok();
    }

    let bears: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| {
            o.name == "Grizzly Bears" && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .collect();

    assert_eq!(
        bears.len(),
        2,
        "expected parent + offspring token, got {bears:?}"
    );
    assert!(bears.iter().any(|o| !o.is_token));
    assert!(bears.iter().any(|o| o.is_token));
}
