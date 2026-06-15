//! Issue #3322 — Airbend must grant the exiled object's owner permission to
//! cast it for the alt cost, even when an opponent controls the airbender.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, PermissionGrantee};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const AANG_ORACLE: &str = "Flash\nFlying\nWhen Aang enters, airbend up to one other target creature or spell. (Exile it. While it's exiled, its owner may cast it for {2} rather than its mana cost.)\nWaterbend {8}: Transform Aang.";

const AIRBEND_CLAUSE: &str = "airbend up to one other target creature or spell";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut engine::game::scenario::GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

#[test]
fn airbend_parser_grants_cast_permission_to_object_owner() {
    let def = parse_effect_chain(AIRBEND_CLAUSE, AbilityKind::Spell);
    let grant = def
        .sub_ability
        .as_ref()
        .expect("airbend must chain a cast permission grant");
    match &*grant.effect {
        Effect::GrantCastingPermission { grantee, .. } => {
            assert_eq!(
                *grantee,
                PermissionGrantee::ObjectOwner,
                "airbend must grant cast permission to each exiled object's owner"
            );
        }
        other => panic!("expected GrantCastingPermission, got {other:?}"),
    }
}

#[test]
fn opponent_airbend_lets_owner_cast_exiled_card_for_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aang = scenario
        .add_creature_to_hand_from_oracle(P1, "Aang, Swift Savior", 2, 3, AANG_ORACLE)
        .id();
    scenario.with_mana_pool(P1, floating_mana(3, ManaType::Blue));

    let mut runner = scenario.build();
    grant_priority(&mut runner, P1);

    runner.cast(aang).target_object(bear).resolve();

    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "airbended creature must be exiled"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&bear),
        "exiled card owner must be able to cast the airbended card for {{2}}"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P1).contains(&bear),
        "airbender's controller must not receive the owner's cast permission"
    );
}
