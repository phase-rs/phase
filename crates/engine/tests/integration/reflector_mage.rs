use crate::support::shared_card_db;
use engine::game::casting::can_cast_object_now;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    ControllerRef, FilterProp, GameRestriction, ProhibitedActivity, RestrictionExpiry,
    RestrictionPlayerScope, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{CastingVariant, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn advance_to_main(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..160 {
        if runner.state().active_player == player && runner.state().phase == Phase::PreCombatMain {
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            ref other => panic!("unexpected turn prompt {other:?}"),
        };
        runner.act(action).unwrap();
    }
    panic!("did not reach {player:?}'s main phase");
}

#[test]
fn departed_mage_token_restricts_only_the_owner_until_the_installers_next_turn() {
    let db = shared_card_db().expect("integration card fixture");
    let controller = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1, controller] {
        scenario.with_library_top(player, &["Island"; 4]);
        for _ in 0..4 {
            scenario.add_basic_land(player, ManaColor::Blue);
        }
    }
    let mage = scenario.add_real_card(P0, "Reflector Mage", Zone::Battlefield, db);
    let copy = scenario.add_real_card(P0, "Cackling Counterpart", Zone::Hand, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    let stolen = scenario
        .add_creature(P1, "Spectral Sailor", 1, 1)
        .controlled_by(controller)
        .id();
    let same = scenario.add_real_card(P1, "Spectral Sailor", Zone::Hand, db);
    let different = scenario.add_real_card(P1, "Opt", Zone::Hand, db);
    let controller_copy = scenario.add_real_card(controller, "Spectral Sailor", Zone::Hand, db);
    let land = scenario.add_real_card(P1, "Island", Zone::Hand, db);
    let mut runner = scenario.build();
    assert!(can_cast_object_now(runner.state(), P1, same));
    let outcome = runner.cast(copy).target_objects(&[mage, stolen]).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().players[P1.0 as usize].hand.contains(&stolen));
    let [GameRestriction::ProhibitActivity {
        source: token,
        affected_players: RestrictionPlayerScope::SpecificPlayer(owner),
        expiry: RestrictionExpiry::UntilPlayerNextTurn { player },
        activity:
            ProhibitedActivity::CastSpells {
                spell_filter: Some(TargetFilter::Typed(filter)),
            },
    }] = runner.state().restrictions.as_slice()
    else {
        panic!("expected one captured restriction")
    };
    assert_eq!((*owner, *player), (P1, P0));
    assert_eq!(
        filter,
        &TypedFilter::default().properties(vec![FilterProp::Named {
            name: "Spectral Sailor".into()
        }])
    );
    let token = *token;
    assert!(runner.state().objects[&token].is_token);
    assert!(!can_cast_object_now(runner.state(), P1, same));
    runner.cast(unsummon).target_object(token).resolve();
    assert!(!runner.state().objects.contains_key(&token));
    advance_to_main(&mut runner, P1);
    assert!(!can_cast_object_now(runner.state(), P1, same));
    assert!(can_cast_object_now(runner.state(), P1, different));
    assert!(can_cast_object_now(
        runner.state(),
        controller,
        controller_copy
    ));
    // CR 601.3: the cast action itself rejects the matching spell.
    assert!(matches!(
        runner.cast(same).try_resolve(),
        Err(EngineError::ActionNotAllowed(reason)) if reason.contains("temporary effect")
    ));
    // CR 305.1 + CR 116.2a: playing a land remains a legal special action.
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id: runner.state().objects[&land].card_id,
        })
        .unwrap();
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    advance_to_main(&mut runner, P0);
    assert!(runner.state().restrictions.is_empty());
    assert!(can_cast_object_now(runner.state(), P1, same));
}

#[test]
fn unresolved_owner_and_source_relative_name_filters_restrict_no_player() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opt = scenario.add_real_card(P0, "Opt", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();
    let absent = ObjectId(9999);
    assert!(!runner.state().objects.contains_key(&absent));
    let named = TypedFilter::default().properties(vec![FilterProp::Named { name: "Opt".into() }]);
    assert!(can_cast_object_now(runner.state(), P0, opt));
    for (scope, filter, can_cast) in [
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.clone(),
            false,
        ),
        (
            RestrictionPlayerScope::ParentObjectTargetOwner,
            named.clone(),
            true,
        ),
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.clone().controller(ControllerRef::You),
            true,
        ),
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.clone().with_type(TypeFilter::Instant),
            true,
        ),
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.properties(vec![
                FilterProp::Named { name: "Opt".into() },
                FilterProp::InZone { zone: Zone::Hand },
            ]),
            true,
        ),
    ] {
        runner.state_mut().restrictions = vec![GameRestriction::ProhibitActivity {
            source: absent,
            affected_players: scope,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::CastSpells {
                spell_filter: Some(TargetFilter::Typed(filter)),
            },
        }];
        assert_eq!(can_cast_object_now(runner.state(), P0, opt), can_cast);
    }
}

#[test]
fn bounced_face_down_creature_shares_no_name_with_its_printed_face_or_another_face_down_cast() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island"; 3]);
        for _ in 0..3 {
            scenario.add_basic_land(player, ManaColor::Blue);
        }
    }
    scenario.add_basic_land(P1, ManaColor::White);
    let morph = scenario.add_real_card(P0, "Willbender", Zone::Hand, db);
    let mage = scenario.add_real_card(P1, "Reflector Mage", Zone::Hand, db);
    let mut runner = scenario.build();
    runner
        .cast(morph)
        .casting_variant(CastingVariant::FaceDown)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert!(runner.state().objects[&morph].face_down);
    advance_to_main(&mut runner, P1);
    let outcome = runner.cast(mage).target_object(morph).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&morph].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&morph].name, "Willbender");
    advance_to_main(&mut runner, P0);
    assert!(matches!(runner.state().restrictions.as_slice(), [
        GameRestriction::ProhibitActivity {
            affected_players: RestrictionPlayerScope::SpecificPlayer(P0),
            activity: ProhibitedActivity::CastSpells { spell_filter: Some(TargetFilter::Typed(filter)) },
            ..
        }
    ] if matches!(filter.properties.as_slice(), [FilterProp::Named { name }] if name.is_empty())));
    // CR 201.2a + CR 708.4: the bounced creature had no name to share with
    // either the printed card or the face-down spell.
    assert!(can_cast_object_now(runner.state(), P0, morph));
    runner
        .cast(morph)
        .casting_variant(CastingVariant::FaceDown)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert!(runner.state().objects[&morph].face_down);
    assert_eq!(runner.state().objects[&morph].zone, Zone::Battlefield);
}
