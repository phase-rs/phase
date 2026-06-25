//! Issue #4001 — Frolicking Familiar // Blow Off Steam: the Adventure instant
//! face must be castable at instant speed outside main phases when only the
//! spell face is affordable.

use engine::game::casting::can_cast_object_now;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    let state = runner.state_mut();
    let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        player_data
            .mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
    }
}

fn blow_off_steam_back_face() -> BackFaceData {
    BackFaceData {
        name: "Blow Off Steam".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types: {
            let mut ct = CardType::default();
            ct.core_types.push(CoreType::Instant);
            ct.subtypes.push("Adventure".to_string());
            ct
        },
        mana_cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        },
        keywords: Vec::new(),
        abilities: vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Red],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: None,
    }
}

fn add_frolicking_familiar_to_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, "Frolicking Familiar", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id()
}

fn setup_frolicking_familiar_adventure(runner: &mut GameRunner, obj_id: ObjectId) {
    let state = runner.state_mut();
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.back_face = Some(blow_off_steam_back_face());
}

#[test]
fn issue_4001_adventure_instant_castable_outside_main_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let obj_id = add_frolicking_familiar_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_frolicking_familiar_adventure(&mut runner, obj_id);
    let card_id = runner.state().objects[&obj_id].card_id;
    // Enough for Blow Off Steam ({1}{R}), not Frolicking Familiar ({2}{R}).
    add_mana(&mut runner, P0, ManaType::Red, 2);

    assert!(
        can_cast_object_now(runner.state(), P0, obj_id),
        "Adventure instant must be castable during declare attackers"
    );

    let result = runner
        .act(GameAction::CastSpell {
            object_id: obj_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed outside main phase");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Adventure { .. },
            } if player == P0
        ),
        "Expected Adventure cast offer outside main phase, got {:?}",
        result.waiting_for
    );
}

#[test]
fn issue_4001_exiled_adventure_creature_does_not_simulate_instant_face() {
    use engine::game::zones::move_to_zone;
    use engine::types::ability::CastingPermission;
    use engine::types::zones::Zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let obj_id = add_frolicking_familiar_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_frolicking_familiar_adventure(&mut runner, obj_id);
    add_mana(&mut runner, P0, ManaType::Red, 2);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), obj_id, Zone::Exile, &mut events);
    runner
        .state_mut()
        .objects
        .get_mut(&obj_id)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::AdventureCreature);

    assert!(
        !can_cast_object_now(runner.state(), P0, obj_id),
        "exiled Adventure creature must not simulate the instant Adventure face outside main phase"
    );
}
