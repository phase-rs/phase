//! Issue #5760 — U.S.Agent, John Walker must attach the Sturdy Shield
//! Equipment token it creates on ETB to itself, not leave it unattached.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const US_AGENT_ORACLE: &str = "When U.S.Agent enters, create a colorless Equipment artifact token named Sturdy Shield with \"Equipped creature gets +1/+2\" and equip {2}. Attach it to U.S.Agent.";

#[test]
fn us_agent_attaches_created_sturdy_shield_on_etb() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let us_agent = scenario
        .add_creature_to_hand_from_oracle(P0, "U.S.Agent, John Walker", 4, 4, US_AGENT_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Human", "Soldier", "Hero"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlack],
            generic: 3,
        })
        .id();

    let mut runner = scenario.build();
    runner.cast(us_agent).resolve();

    let us_agent_obj = runner.state().objects.get(&us_agent).expect("U.S.Agent");
    assert_eq!(
        us_agent_obj.zone,
        Zone::Battlefield,
        "U.S.Agent should enter the battlefield"
    );

    let token_id = *us_agent_obj
        .attachments
        .first()
        .expect("U.S.Agent should have the created Equipment token attached");

    let token = runner
        .state()
        .objects
        .get(&token_id)
        .expect("created Sturdy Shield token");
    assert!(token.is_token, "attachment should be the created token");
    assert_eq!(token.name, "Sturdy Shield");
    assert_eq!(
        token.attached_to,
        Some(AttachTarget::Object(us_agent)),
        "the Sturdy Shield token should be attached to U.S.Agent"
    );
}
