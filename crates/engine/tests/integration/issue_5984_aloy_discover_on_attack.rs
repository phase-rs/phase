//! Regression for issue #5984: Aloy, Savior of Meridian's attack trigger must
//! resolve discover X end-to-end, binding X to the greatest power among the
//! attacking artifact creatures (CR 603.2c + CR 701.57a).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const ALOY_ORACLE: &str = "Vigilance, reach\nWhenever one or more artifact creatures you control attack, discover X, where X is the greatest power among them.";

fn add_artifact_creature(
    state: &mut engine::types::game_state::GameState,
    player: PlayerId,
    card_id: u64,
    name: &str,
    power: i32,
    toughness: i32,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types = CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Artifact, CoreType::Creature],
        subtypes: vec!["Myr".to_string()],
    };
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    obj.summoning_sick = false;
    id
}

/// CR 508.1 + CR 603.2c + CR 701.57a: attacking artifact creatures discover
/// with X equal to the greatest power among the matching attackers.
#[test]
fn aloy_discover_binds_x_to_greatest_artifact_attacker_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aloy = scenario
        .add_creature_from_oracle(P0, "Aloy, Savior of Meridian", 3, 5, ALOY_ORACLE)
        .id();
    let bystander = scenario.add_creature(P0, "Bystander", 9, 9).id();
    let hit = scenario
        .add_spell_to_library_top(P0, "MV4 Hit", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario
        .add_spell_to_library_top(P0, "Land A", false)
        .as_land();
    scenario
        .add_spell_to_library_top(P0, "Land B", false)
        .as_land();

    let mut runner = scenario.build();
    let weak = add_artifact_creature(runner.state_mut(), P0, 2001, "Myr Scout", 2, 2);
    let strong = add_artifact_creature(runner.state_mut(), P0, 2002, "Myr Enforcer", 4, 4);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (weak, AttackTarget::Player(P1)),
            (strong, AttackTarget::Player(P1)),
            (aloy, AttackTarget::Player(P1)),
            (bystander, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, discover_value, .. },
                ..
            } if hit_card == hit && discover_value == 4
        ),
        "expected discover 4 CastOffer for the MV4 hit; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline discover (keep in hand)");

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Hand,
        "declined discover hit goes to hand (CR 701.57a)"
    );
    assert!(
        runner.state().stack.is_empty(),
        "discover must finish resolving the attack trigger before combat continues"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after discover completes, priority returns; got {:?}",
        runner.state().waiting_for
    );
}

/// Same pipeline using hydrated card-db Aloy (production parse path).
#[test]
fn aloy_discover_resolves_from_card_db() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aloy = scenario.add_real_card(P0, "Aloy, Savior of Meridian", Zone::Battlefield, db);
    let hit = scenario
        .add_spell_to_library_top(P0, "MV3 Hit", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let attacker = add_artifact_creature(runner.state_mut(), P0, 2003, "Ornithopter", 3, 3);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, discover_value, .. },
                ..
            } if hit_card == hit && discover_value == 3
        ),
        "card-db Aloy must discover 3 off a power-3 artifact attack; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().objects.contains_key(&aloy),
        "Aloy stays on the battlefield while discover resolves"
    );
}
