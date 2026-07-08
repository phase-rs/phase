//! Integration tests for Ninjutsu cluster issues #3648, #3662, #3661
//!
//! - #3648: Ninjutsu not being offered from the command zone
//! - #3662: Sakashima's Student paying/returning a creature but not entering
//! - #3661: Turtle Lair mana not making a Ninja spell castable (NOT A BUG - see below)

use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn test_3648_commander_ninjutsu_offered_from_command_zone() {
    // CR 702.49d: Commander ninjutsu functions from the command zone
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);

    // Set up an unblocked attacker
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();

    // Add a regular Ninjutsu card to hand
    let hand_ninja = scenario.add_creature_to_hand(P0, "Hand Ninja", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    // Configure the hand Ninja with regular Ninjutsu
    {
        let obj = runner.state_mut().objects.get_mut(&hand_ninja).unwrap();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        obj.keywords.push(Keyword::Ninjutsu(cost.clone()));
        obj.base_keywords.push(Keyword::Ninjutsu(cost));
    }

    // Add a Commander Ninjutsu card to command zone using create_object helper
    let commander_ninja_id = zones::create_object(
        runner.state_mut(),
        CardId(999),
        P0,
        "Commander Ninja".to_string(),
        Zone::Command,
    );
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&commander_ninja_id)
            .unwrap();
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        obj.keywords.push(Keyword::CommanderNinjutsu(cost.clone()));
        obj.base_keywords.push(Keyword::CommanderNinjutsu(cost));
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Ninja".to_string());
        obj.is_commander = true;
    }

    // Set up combat state
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
        ..Default::default()
    });
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.state_mut().priority_player = P0;

    // Add mana for Ninjutsu cost
    {
        let state = runner.state_mut();
        let player_data = state.players.iter_mut().find(|p| p.id == P0).unwrap();
        for _ in 0..2 {
            player_data.mana_pool.add(ManaUnit::new(
                ManaType::Blue,
                ObjectId(0),
                false,
                Vec::new(),
            ));
        }
    }

    // Get legal actions for P0
    let actions = engine::ai_support::legal_actions(runner.state());

    // Check if ActivateNinjutsu is available for BOTH regular and Commander Ninjutsu.
    let ninjutsu_actions: Vec<_> = actions
        .iter()
        .filter_map(|action| {
            if let GameAction::ActivateNinjutsu {
                ninjutsu_object_id,
                creature_to_return,
            } = action
            {
                Some((*ninjutsu_object_id, *creature_to_return))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        ninjutsu_actions,
        vec![(hand_ninja, attacker), (commander_ninja_id, attacker)],
        "regular and commander ninjutsu must both be offered for the unblocked attacker"
    );
}
