//! Thor, God of Thunder — cast-time mana value for X spells.
//!
//! The trigger's "that spell's mana value" must use the value recorded when the
//! spell was cast, including announced X, rather than the off-stack printed value.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::{CastingVariant, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THOR_ORACLE: &str = "Flying\nWhen Thor enters, exile target Equipment, instant, or sorcery card from your graveyard. Until the end of your next turn, you may play that card.\nWhenever you cast a noncreature spell, Thor deals damage equal to that spell's mana value to any target.";

const FORTH_EORLINGAS_ORACLE: &str = "Create X 2/2 red Human Knight creature tokens with trample and haste.\nWhenever one or more creatures you control deal combat damage to one or more players this turn, you become the monarch.";

const COUNTERSPELL_ORACLE: &str = "Counter target spell.";

const DEVILS_PLAY_ORACLE: &str = "Devil's Play deals X damage to any target.\nFlashback {X}{R}{R}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

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
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
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
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let mut committed = runner.cast(forth).x(4).target_objects(&[victim]).commit();

    // CR 117.7 + CR 701.6a: answer the active player's priority, then cast a
    // real Counterspell in response. Its normal resolution path moves Forth
    // Eorlingas! to the graveyard through the replacement-aware pipeline while
    // Thor's already-triggered ability remains below the counterspell.
    committed
        .act(engine::types::actions::GameAction::PassPriority)
        .expect("P0 passes priority to the counterspell controller");
    let outcome = committed.cast(counterspell).target_object(forth).resolve();

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

/// CR 603.7c + CR 202.3e: Each SpellCast trigger must retain the mana value of
/// its own cast when the same object is cast again before the earlier trigger
/// resolves. This uses a legal Counterspell response and a real Flashback cast
/// rather than mutating the stack or zones directly.
#[test]
fn thor_binds_same_object_recasts_to_their_own_cast_values() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Thor, God of Thunder", 5, 5, THOR_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(
        P0,
        "Leyline of Anticipation",
        "You may cast spells as though they had flash.",
    );
    let first_target = scenario.add_creature(P1, "First Thor Target", 2, 20).id();
    let second_target = scenario.add_creature(P1, "Second Thor Target", 2, 20).id();
    let devil = scenario
        .add_spell_to_hand_from_oracle(P0, "Devil's Play", false, DEVILS_PLAY_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
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
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let mut first_commit = runner.cast(devil).x(4).target_object(first_target).commit();
    first_commit
        .act(engine::types::actions::GameAction::PassPriority)
        .expect("P0 passes priority to Counterspell");

    {
        let mut counter_commit = first_commit
            .cast(counterspell)
            .target_object(devil)
            .commit();
        while counter_commit
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == counterspell)
        {
            assert!(
                matches!(
                    counter_commit.state().waiting_for,
                    WaitingFor::Priority { .. }
                ),
                "Counterspell must resolve through priority, got {:?}",
                counter_commit.state().waiting_for
            );
            counter_commit
                .act(engine::types::actions::GameAction::PassPriority)
                .expect("pass priority while Counterspell resolves");
        }
    }

    let outcome = first_commit
        .cast(devil)
        .casting_variant(CastingVariant::Flashback)
        .x(1)
        .target_object(second_target)
        .resolve();

    assert_eq!(
        outcome.damage_marked(first_target),
        5,
        "the first Thor trigger must retain Devil's Play X=4 mana value 5"
    );
    assert_eq!(
        outcome.damage_marked(second_target),
        3,
        "the recast's Thor trigger (MV 2) plus Devil's Play X=1 must total 3"
    );
    assert_eq!(
        outcome.zone_of(devil),
        Zone::Exile,
        "the Flashback recast must use its legal exile replacement"
    );
}
