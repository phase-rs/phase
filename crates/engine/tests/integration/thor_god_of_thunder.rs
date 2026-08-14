//! Thor, God of Thunder — cast-time mana value for X spells.
//!
//! The trigger's "that spell's mana value" must use the value recorded when the
//! spell was cast, including announced X, rather than the off-stack printed value.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THOR_ORACLE: &str = "Flying\nWhen Thor enters, exile target Equipment, instant, or sorcery card from your graveyard. Until the end of your next turn, you may play that card.\nWhenever you cast a noncreature spell, Thor deals damage equal to that spell's mana value to any target.";

const FORTH_EORLINGAS_ORACLE: &str = "Create X 2/2 red Human Knight creature tokens with trample and haste.\nWhenever one or more creatures you control deal combat damage to one or more players this turn, you become the monarch.";

#[test]
fn thor_deals_cast_time_mana_value_to_target_for_x_spell() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Thor, God of Thunder", 5, 5, THOR_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Target Dummy", 2, 12).id();
    let forth = scenario
        .add_spell_to_hand_from_oracle(P0, "Forth Eorlingas!", false, FORTH_EORLINGAS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let mut committed = runner.cast(forth).x(4).target_objects(&[victim]).commit();

    // Keep Thor's already-triggered ability on the stack, but model the
    // triggering spell leaving the stack before that ability resolves. This
    // is the failing off-stack shape from the supplied replay.
    let mut zone_events = Vec::new();
    engine::game::zones::move_to_zone(
        committed.state_mut(),
        forth,
        Zone::Graveyard,
        &mut zone_events,
    );
    committed
        .state_mut()
        .stack
        .retain(|entry| entry.source_id != forth);
    let cast_record = committed
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| {
            records
                .iter()
                .find(|record| record.spell_object_id == Some(forth))
        })
        .expect("cast history must retain the triggering spell after it leaves the stack");
    assert_eq!(
        cast_record.mana_value, 6,
        "the cast-time record must retain Forth Eorlingas!'s X=4 mana value"
    );

    let outcome = committed.resolve();

    assert_eq!(
        outcome.damage_marked(victim),
        6,
        "Thor must use Forth Eorlingas!'s cast-time mana value {{X}}{{R}}{{W}} with X=4"
    );
    assert_eq!(
        outcome.zone_of(forth),
        Zone::Graveyard,
        "the triggering spell must have left the stack by the end of the cast pipeline"
    );
}
