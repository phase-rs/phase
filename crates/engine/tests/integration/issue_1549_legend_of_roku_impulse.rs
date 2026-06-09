//! Regression test for GitHub issue #1549 — The Legend of Roku chapter I.
//!
//! Chapter I: "Exile the top three cards of your library. Until the end of
//! your next turn, you may play those cards."
//!
//! CR 400.7i + CR 603.7: The exile step publishes a tracked set; the chained
//! `GrantCastingPermission { PlayFromExile }` must attach to those three
//! objects so they surface as castable/playable from exile.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::casting::{exile_lands_playable_by_permission, spell_objects_available_to_cast};
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, CastingPermission};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::zones::Zone;

fn roku_chapter_one_execute() -> engine::types::ability::AbilityDefinition {
    parse_effect_chain(
        "Exile the top three cards of your library. Until the end of your next turn, you may play those cards.",
        AbilityKind::Spell,
    )
}

#[test]
fn legend_of_roku_chapter_one_grants_play_from_exile_on_exiled_cards() {
    let execute = roku_chapter_one_execute();

    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let saga = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(1),
            P0,
            "The Legend of Roku".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.counters.insert(CounterType::Lore, 1);
        id
    };

    let mut lib_cards = Vec::new();
    for (idx, (name, is_land)) in [
        ("Exiled Bolt", false),
        ("Exiled Island", true),
        ("Exiled Bear", false),
    ]
    .into_iter()
    .enumerate()
    {
        let card_id = CardId(10 + idx as u64);
        let id = {
            let state = runner.state_mut();
            let id = create_object(state, card_id, P0, name.to_string(), Zone::Library);
            let obj = state.objects.get_mut(&id).unwrap();
            if is_land {
                obj.card_types.core_types = vec![CoreType::Land];
            } else {
                obj.card_types.core_types.push(CoreType::Instant);
                obj.mana_cost = if idx == 0 {
                    ManaCost::Cost {
                        shards: vec![ManaCostShard::Red],
                        generic: 0,
                    }
                } else {
                    ManaCost::Cost {
                        shards: vec![ManaCostShard::Green],
                        generic: 1,
                    }
                };
            }
            id
        };
        lib_cards.push((id, is_land));
    }

    let resolved = build_resolved_from_def(&execute, saga, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter I resolution");

    let state = runner.state();
    for (obj_id, is_land) in &lib_cards {
        let obj = state.objects.get(obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Exile, "{obj_id:?} should be exiled");
        assert_eq!(
            obj.casting_permissions.len(),
            1,
            "{obj_id:?} should carry exactly one play permission"
        );
        match &obj.casting_permissions[0] {
            CastingPermission::PlayFromExile { granted_to, .. } => {
                assert_eq!(*granted_to, P0);
            }
            other => panic!("expected PlayFromExile, got {other:?}"),
        }
        if *is_land {
            assert!(
                !spell_objects_available_to_cast(state, P0).contains(obj_id),
                "lands must not surface on the cast path"
            );
        } else {
            assert!(
                spell_objects_available_to_cast(state, P0).contains(obj_id),
                "exiled spell {obj_id:?} must be castable"
            );
        }
    }

    let land_id = lib_cards.iter().find(|(_, is_land)| *is_land).unwrap().0;
    assert!(
        exile_lands_playable_by_permission(state, P0)
            .iter()
            .any(|(id, _)| *id == land_id),
        "exiled land must be playable via the play-land path"
    );
}
