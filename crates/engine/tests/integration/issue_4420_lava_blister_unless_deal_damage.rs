//! Regression for issue #4420: Lava Blister's unless-have-deal-damage alternative
//! must deal damage to the land's controller and suppress the destroy effect when
//! paid.
//!
//! https://github.com/phase-rs/phase/issues/4420

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LAVA_BLISTER_ORACLE: &str =
    "Destroy target nonbasic land unless its controller has Lava Blister deal 6 damage to them.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn add_nonbasic_land(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> ObjectId {
    let land = create_object(
        runner.state_mut(),
        CardId(800),
        player,
        "Wasteland".to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&land).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    land
}

fn cast_lava_blister_to_unless_prompt(
    runner: &mut engine::game::scenario::GameRunner,
    blister: ObjectId,
    land: ObjectId,
) {
    runner
        .act(GameAction::CastSpell {
            object_id: blister,
            card_id: runner.state().objects[&blister].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Lava Blister");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Object(land)],
                    })
                    .expect("target P1's nonbasic land");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected pre-resolution prompt: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Lava Blister must offer the land controller an unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
}

fn setup_at_unless_prompt() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blister = scenario
        .add_spell_to_hand_from_oracle(P0, "Lava Blister", false, LAVA_BLISTER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let land = add_nonbasic_land(&mut runner, P1);
    add_mana(&mut runner, &[ManaType::Colorless, ManaType::Red]);
    cast_lava_blister_to_unless_prompt(&mut runner, blister, land);
    (runner, blister, land)
}

#[test]
fn lava_blister_declined_unless_payment_destroys_targeted_land() {
    let (mut runner, _blister, land) = setup_at_unless_prompt();
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline damage alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "declining the unless cost must destroy the targeted nonbasic land"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life,
        20,
        "declining the unless cost must not deal damage to the land's controller"
    );
}

#[test]
fn lava_blister_paid_unless_payment_deals_damage_and_spares_land() {
    let (mut runner, _blister, land) = setup_at_unless_prompt();
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("accept damage alternative");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life,
        14,
        "paying the unless cost must have Lava Blister deal 6 damage to the land's controller"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "paying the unless cost must prevent the destroy effect"
    );
}
