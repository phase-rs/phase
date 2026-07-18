//! Regression for issue #6010: an opponent-owned permanent spell cast through
//! Ragavan, Nimble Pilferer's combat-damage permission must enter under the
//! Ragavan player's control, not its owner's control.
//!
//! https://github.com/phase-rs/phase/issues/6010

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{CastingPermission, Duration};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RAGAVAN_ORACLE: &str = "Whenever Ragavan deals combat damage to a player, create a Treasure token and exile the top card of that player's library. Until end of turn, you may cast that card.\n\
Dash {1}{R} (You may cast this spell for its dash cost. If you do, it gains haste, and it's returned from the battlefield to its owner's hand at the beginning of the next end step.)";

fn add_zero_cost_flash_creature_to_library_top(
    runner: &mut GameRunner,
    player: PlayerId,
    name: &str,
) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, player, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).expect("library card exists");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.keywords.push(Keyword::Flash);
    obj.base_keywords.push(Keyword::Flash);
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    obj.mana_cost = ManaCost::zero();
    obj.base_mana_cost = ManaCost::zero();

    let player_state = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    player_state.library.retain(|&object_id| object_id != id);
    player_state.library.insert(0, id);
    id
}

#[test]
fn ragavan_casts_opponent_owned_exiled_creature_under_casters_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ragavan = scenario
        .add_creature_from_oracle(P0, "Ragavan, Nimble Pilferer", 2, 1, RAGAVAN_ORACLE)
        .id();
    let mut runner = scenario.build();
    let stolen = add_zero_cost_flash_creature_to_library_top(&mut runner, P1, "Nimble Pilferer");

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(ragavan, AttackTarget::Player(P1))])
        .expect("declare Ragavan attacking P1");
    runner.pass_both_players();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
    }
    runner.combat_damage();

    let stolen_obj = runner
        .state()
        .objects
        .get(&stolen)
        .expect("the exiled opponent card must still exist");
    assert_eq!(
        stolen_obj.zone,
        Zone::Exile,
        "Ragavan must exile the top card of the damaged player's library"
    );
    assert!(
        stolen_obj
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    granted_to,
                    ..
                } if *granted_to == P0
            )),
        "Ragavan must grant its controller permission to cast the exiled card; got {:?}",
        stolen_obj.casting_permissions
    );

    let card_id = stolen_obj.card_id;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.state_mut().priority_player = P0;
    runner
        .act(GameAction::CastSpell {
            object_id: stolen,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the opponent-owned card using Ragavan's permission");
    runner.advance_until_stack_empty();

    let stolen_obj = runner
        .state()
        .objects
        .get(&stolen)
        .expect("the cast creature must still exist");
    assert_eq!(
        stolen_obj.zone,
        Zone::Battlefield,
        "the opponent-owned creature must resolve onto the battlefield"
    );
    assert_eq!(
        stolen_obj.owner, P1,
        "the physical card remains owned by P1"
    );
    assert_eq!(
        stolen_obj.controller, P0,
        "CR 608.3a: the permanent enters under the spell controller's control"
    );
}
