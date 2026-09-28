//! Fuse integration coverage against real split-card fixture data.
//!
//! `Breaking // Entering` is useful here because it exercises the real three
//! choices: Breaking, Entering, and the fused spell. Entering's target is
//! selected while casting, so it must already be in a graveyard; Breaking then
//! mills its independent library markers as the fused spell resolves.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, CastingVariantFace, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn pool_units(mana: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = ObjectId(0);
    mana.iter()
        .map(|m| ManaUnit::new(*m, dummy, false, vec![]))
        .collect()
}

fn assert_breaking_entering_identity(
    state: &engine::types::game_state::GameState,
    breaking: ObjectId,
) {
    let card = &state.objects[&breaking];
    assert_eq!(card.name, "Breaking");
    assert_eq!(
        card.back_face.as_ref().map(|face| face.name.as_str()),
        Some("Entering"),
        "the real database fixture must retain Entering as Breaking's split half"
    );
}

#[test]
fn hand_fuse_fused_left_combines_cost_characteristics_and_resolves_both_halves() {
    let db = load_db().expect("fuse runtime coverage requires the real card database");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    let reanimation_target = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    let library_markers = [
        "Lightning Bolt",
        "Opt",
        "Divination",
        "Doom Blade",
        "Shock",
        "Unsummon",
        "Negate",
        "Cancel",
    ]
    .map(|name| scenario.add_real_card(P1, name, Zone::Library, db));
    scenario.with_mana_pool(
        P0,
        pool_units(&[
            ManaType::Blue,
            ManaType::Black,
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_breaking_entering_identity(runner.state(), breaking);

    let commit = runner
        .cast(breaking)
        .casting_variant(CastingVariant::Fuse)
        .target_player(P1)
        .target_object(reanimation_target)
        .commit();

    let selected = commit
        .selected_casting_variant()
        .expect("fuse should be selected through CastingVariantChoice");
    assert_eq!(selected.variant, CastingVariant::Fuse);
    assert_eq!(selected.face, CastingVariantFace::Left);
    assert_eq!(
        selected.mana_cost.mana_value(),
        8,
        "CR 702.102c: fused choice cost includes both halves"
    );

    let state = commit.state();
    assert_eq!(
        state.players[0].mana_pool.total(),
        0,
        "the exact fused cost should be paid before the spell reaches priority"
    );
    assert_eq!(state.stack.len(), 1, "fused spell should be on the stack");
    let stack_entry = state.stack.last().unwrap();
    let StackEntryKind::Spell {
        casting_variant,
        actual_mana_spent,
        ..
    } = &stack_entry.kind
    else {
        panic!("expected fused split card to be a spell stack entry");
    };
    assert_eq!(*casting_variant, CastingVariant::Fuse);
    assert_eq!(
        *actual_mana_spent, 8,
        "CR 702.102c: fused total cost includes both halves"
    );

    let stack_object = &state.objects[&stack_entry.source_id];
    assert!(stack_object
        .card_types
        .core_types
        .contains(&CoreType::Sorcery));
    assert!(stack_object.color.contains(&ManaColor::Blue));
    assert!(stack_object.color.contains(&ManaColor::Black));
    assert!(stack_object.color.contains(&ManaColor::Red));
    assert_eq!(
        stack_object.zone,
        Zone::Stack,
        "CR 702.102b + CR 709.4d: fused characteristics must be visible on stack"
    );
    assert_eq!(
        state.objects[&reanimation_target].zone,
        Zone::Graveyard,
        "Entering's target must be legal when the fused spell is cast"
    );
    let outcome = commit.resolve();

    // CR 608.2c + CR 702.102d: Breaking mills its eight markers, then Entering
    // returns the creature card that was legally targeted during casting.
    outcome.assert_zone(&library_markers, Zone::Graveyard);
    outcome.assert_zone(&[reanimation_target], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&reanimation_target].controller, P0);
    outcome.assert_zone(&[breaking], Zone::Graveyard);
}

#[test]
fn hand_fuse_normal_left_casts_only_breaking() {
    let db = load_db().expect("fuse runtime coverage requires the real card database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    let library_markers = [
        "Lightning Bolt",
        "Opt",
        "Divination",
        "Doom Blade",
        "Shock",
        "Unsummon",
        "Negate",
        "Cancel",
    ]
    .map(|name| scenario.add_real_card(P1, name, Zone::Library, db));
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Blue, ManaType::Black]));
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_breaking_entering_identity(runner.state(), breaking);

    let outcome = runner
        .cast(breaking)
        .casting_variant_face(CastingVariant::Normal, CastingVariantFace::Left)
        .target_player(P1)
        .resolve();
    outcome.assert_zone(&[breaking], Zone::Graveyard);
    outcome.assert_zone(&library_markers, Zone::Graveyard);
    assert_eq!(outcome.state().players[1].graveyard.len(), 8);
}

#[test]
fn hand_fuse_normal_right_casts_only_entering_with_preexisting_graveyard_creature() {
    let db = load_db().expect("fuse runtime coverage requires the real card database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    let creature = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        pool_units(&[
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_breaking_entering_identity(runner.state(), breaking);

    let outcome = runner
        .cast(breaking)
        .casting_variant_face(CastingVariant::Normal, CastingVariantFace::Right)
        .target_object(creature)
        .resolve();
    outcome.assert_zone(&[creature], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&creature].controller, P0);
    outcome.assert_zone(&[breaking], Zone::Graveyard);
}

/// Regression for PR #3687 review: spell//spell split cards with Fuse must keep
/// the `CastingVariant::Fuse` prompt — not the Life // Death ModalFaceChoice path.
#[test]
fn fuse_split_card_uses_casting_variant_choice_not_modal_face_choice() {
    let db = load_db().expect("fuse runtime coverage requires the real card database");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        pool_units(&[ManaType::Blue, ManaType::Black, ManaType::Red]),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&breaking].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: breaking,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("CastSpell Breaking");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ModalFaceChoice { .. }
        ),
        "Fuse split cards must not use ModalFaceChoice; got {:?}",
        runner.state().waiting_for
    );
}
