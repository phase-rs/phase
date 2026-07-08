//! Regression test for GitHub issue #751 — Ark of Hunger / Tablet of Discovery
//! ("{T}: Mill a card. You may play that card this turn.").
//!
//! CR 701.17d: When a mill effect grants permission to play "that card", the
//! permission applies to the milled card — which lives in the GRAVEYARD
//! (CR 701.17a/c, a public zone), not in exile. The parser emits
//! `Mill (→ Graveyard)` chained to `GrantCastingPermission { PlayFromExile,
//! TrackedSet 0 }`, so the grant lands on a graveyard object. Before the fix
//! every `PlayFromExile` consult site hard-gated on `Zone::Exile`, so the
//! milled card carried a live-but-never-consulted permission and was reported
//! "not in a castable zone".
//!
//! The fix makes the three object-tagged `PlayFromExile` consult sites
//! zone-aware (exile OR graveyard, lands excluded from the cast path per
//! CR 305.1). The battlefield-static exile-cast path (Maralen / The Matrix of
//! Time) stays exile-only by rule (CR 113.6b), which the Advanced
//! Reconstruction case below asserts.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::casting::{
    graveyard_lands_playable_by_permission, spell_objects_available_to_cast,
};
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::prune_end_of_turn_casting_permissions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, CastingPermission, Duration};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::zones::{EtbTapState, Zone};

/// Ark of Hunger's `{T}` ability chain: `Mill a card. You may play that card
/// this turn.` The parser emits `Mill (→ Graveyard)` then
/// `GrantCastingPermission { PlayFromExile UntilEndOfTurn, TrackedSet 0 }`.
fn ark_of_hunger_mill_grant_chain() -> engine::types::ability::AbilityDefinition {
    parse_effect_chain(
        "Mill a card. You may play that card this turn.",
        AbilityKind::Activated,
    )
}

/// Build a battlefield Ark-of-Hunger-like source plus a single library card,
/// drive the real `Mill → GrantCastingPermission` chain, and return the runner
/// with the milled card now in the graveyard carrying `PlayFromExile`.
fn mill_one_and_grant(
    runner: &mut GameRunner,
    library_card_name: &str,
    is_land: bool,
    mana_cost: ManaCost,
) -> engine::types::identifiers::ObjectId {
    let source = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(1),
            P0,
            "Ark of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        id
    };

    let milled = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(2),
            P0,
            library_card_name.to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        if is_land {
            obj.card_types.core_types = vec![CoreType::Land];
        } else {
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = mana_cost;
        }
        id
    };

    let chain = ark_of_hunger_mill_grant_chain();
    let resolved = build_resolved_from_def(&chain, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Ark of Hunger mill + grant resolution");

    milled
}

/// Primary fix: a milled NON-LAND card lands in the graveyard, carries a live
/// `PlayFromExile`, surfaces on the cast path, and actually casts from the
/// graveyard through the pipeline.
///
/// Discriminating assertion (flips when the fix is reverted): the milled card
/// MUST appear in `spell_objects_available_to_cast(state, P0)`. Pre-fix this
/// returns an empty set for the graveyard card (the consult site gated on
/// `Zone::Exile`), and the subsequent `runner.cast(...)` would fail with
/// "Card is not in a castable zone".
#[test]
fn milled_card_is_castable_from_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Red,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    let milled = mill_one_and_grant(
        &mut runner,
        "Milled Bear",
        false,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        },
    );

    // CR 701.17a/c: the milled card is in the graveyard (public zone), NOT exile.
    assert_eq!(
        runner.state().objects[&milled].zone,
        Zone::Graveyard,
        "milled card must be in the graveyard"
    );
    // CR 701.17d: the grant attached `PlayFromExile` to the graveyard card.
    assert!(
        runner.state().objects[&milled]
            .casting_permissions
            .iter()
            .any(|p| matches!(
                p,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    granted_to,
                    ..
                } if *granted_to == P0
            )),
        "milled card must carry PlayFromExile (UntilEndOfTurn) for P0; got {:?}",
        runner.state().objects[&milled].casting_permissions
    );

    // DISCRIMINATING ASSERTION — flips when the fix is reverted.
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&milled),
        "milled non-land card must surface on the cast path from the graveyard (#751)"
    );

    // Drive the real cast pipeline: graveyard -> stack.
    runner.state_mut().phase = engine::types::phase::Phase::PreCombatMain;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0 };
    runner.cast(milled).resolve();
    assert_ne!(
        runner.state().objects[&milled].zone,
        Zone::Graveyard,
        "casting from the graveyard must move the card out of the graveyard \
         (graveyard -> stack -> battlefield)"
    );
}

/// Land variant: a milled LAND with `PlayFromExile` is playable via the
/// land-play path (`graveyard_lands_playable_by_permission`) and MUST NOT reach
/// the spell-cast path (CR 305.1 — lands are played, not cast).
#[test]
fn milled_land_is_playable_via_land_path_not_cast_path() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    let milled = mill_one_and_grant(&mut runner, "Milled Island", true, ManaCost::NoCost);

    assert_eq!(runner.state().objects[&milled].zone, Zone::Graveyard);

    // CR 305.1: lands never enter the cast path.
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&milled),
        "milled land must NOT surface on the spell-cast path (CR 305.1)"
    );

    // DISCRIMINATING ASSERTION — flips when the fix is reverted: the milled land
    // is playable via the graveyard land-play sweep.
    assert!(
        graveyard_lands_playable_by_permission(runner.state(), P0)
            .iter()
            .any(|(id, _)| *id == milled),
        "milled land must be playable from the graveyard via the land-play path (#751)"
    );

    // Drive the play-land action through the real pipeline.
    let card_id = runner.state().objects[&milled].card_id;
    runner.state_mut().phase = engine::types::phase::Phase::PreCombatMain;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner
        .act(GameAction::PlayLand {
            object_id: milled,
            card_id,
        })
        .expect("milled land must be playable via the land-play special action");
    assert_eq!(
        runner.state().objects[&milled].zone,
        Zone::Battlefield,
        "playing the milled land must move it to the battlefield"
    );
}

/// Regression: Advanced Reconstruction mills, then EXILES a graveyard card and
/// grants `PlayFromExile` on the EXILED card. That card lives in exile, so it
/// must continue to surface only on the exile path. The new graveyard gate
/// (`Zone::Graveyard` AND live `PlayFromExile`) must NOT over-broaden to admit
/// it twice or relocate it off the exile path.
///
/// This proves the zone gate is `exile OR graveyard`, not "any zone": a
/// `PlayFromExile`-tagged card sitting in EXILE is still recognized exactly as
/// before, and a `PlayFromExile`-tagged card sitting in the GRAVEYARD is the
/// only new admission.
#[test]
fn exiled_card_with_play_permission_stays_on_exile_path() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    // Mirror Advanced Reconstruction's end state: a non-land card in EXILE
    // carrying PlayFromExile for P0 (its ChangeZone(-> Exile) resolved the
    // TrackedSet sentinel to a card in exile).
    let exiled = {
        let state = runner.state_mut();
        let id = create_object(state, CardId(3), P0, "Exiled Bear".to_string(), Zone::Exile);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
        obj.casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                granted_to: P0,
                frequency: engine::types::statics::CastFrequency::Unlimited,
                source_id: None,
                invalidation: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: EtbTapState::Unspecified,
            });
        id
    };

    // The exiled card still surfaces on the cast path (unchanged behavior).
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&exiled),
        "an exiled card with PlayFromExile must remain castable from exile"
    );
    // It is NOT in the graveyard, so the new graveyard land sweep must ignore it.
    assert!(
        !graveyard_lands_playable_by_permission(runner.state(), P0)
            .iter()
            .any(|(id, _)| *id == exiled),
        "the exiled card must not appear in the graveyard land-play sweep"
    );
}

/// Duration prune: a graveyard object's `UntilEndOfTurn` `PlayFromExile` must be
/// removed at cleanup (CR 514.2), so the milled card is no longer castable from
/// the graveyard after the turn ends.
#[test]
fn graveyard_play_permission_is_pruned_at_end_of_turn() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    let milled = mill_one_and_grant(
        &mut runner,
        "Milled Bear",
        false,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        },
    );

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&milled),
        "milled card must be castable before cleanup"
    );

    // CR 514.2: cleanup prunes UntilEndOfTurn casting permissions, zone-agnostic.
    prune_end_of_turn_casting_permissions(runner.state_mut());

    assert!(
        runner.state().objects[&milled]
            .casting_permissions
            .is_empty(),
        "UntilEndOfTurn PlayFromExile on a graveyard object must be pruned at cleanup"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&milled),
        "after cleanup the milled card must no longer be castable from the graveyard"
    );
}
