//! Runtime tests for Meld (CR 701.42 / CR 712.4). Declared from `game/mod.rs`
//! so the resolver (`game/meld.rs`) stays implementation-only.
//!
//! These drive the real resolve pipeline (`perform_meld` against a
//! `GameScenario`-built state) and would FAIL if the meld effect were reverted —
//! they are regression tests, not AST-shape tests. They exercise the building
//! block: exile-both → single melded permanent presenting the result face →
//! leave-split back to front faces → transform prohibition → ETB firing.

use std::sync::Arc;

use crate::game::meld::perform_meld;
use crate::game::scenario::{GameScenario, P0, P1};
use crate::types::ability::{Effect, PtValue, ResolvedAbility};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::WaitingFor;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

const RESULT_NAME: &str = "Brisela, Voice of Nightmares";

/// Build a result `CardFace` (Brisela, 9/10 Legendary Angel Horror) and seed it
/// into the registry under its lowercase key (the path `walk_effect` →
/// `build_conjure_registry` populates in production).
fn seed_result_face(state: &mut crate::types::game_state::GameState) {
    let mut face = CardFace {
        name: RESULT_NAME.to_string(),
        power: Some(PtValue::Fixed(9)),
        toughness: Some(PtValue::Fixed(10)),
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    let registry = Arc::make_mut(&mut state.card_face_registry);
    registry.insert(RESULT_NAME.to_lowercase(), face);
    seed_meld_pair(
        state,
        "Gisela, the Broken Blade",
        "Bruna, the Fading Light",
        RESULT_NAME,
    );
}

fn seed_meld_pair(
    state: &mut crate::types::game_state::GameState,
    source: &str,
    partner: &str,
    result: &str,
) {
    let key = format!("{}\0{}", source.to_lowercase(), partner.to_lowercase());
    Arc::make_mut(&mut state.meld_pair_registry).insert(
        key,
        crate::types::game_state::MeldPairRecord {
            source: source.to_string(),
            partner: partner.to_string(),
            result: result.to_string(),
        },
    );
}

/// A meld `ResolvedAbility` whose source is `source`, controlled by `controller`,
/// melding with `partner` into Brisela.
fn meld_ability(source: ObjectId, controller: PlayerId, partner: &str) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Meld {
            source: "Gisela, the Broken Blade".to_string(),
            partner: partner.to_string(),
            result: RESULT_NAME.to_string(),
            source_filter: crate::types::ability::TargetFilter::SelfRef,
            partner_filter: crate::types::ability::TargetFilter::Any,
            entry: crate::types::ability::PermanentEntryMode::Normal,
        },
        Vec::new(),
        source,
        controller,
    )
}

/// A meld `ResolvedAbility` with an explicit expected source name.
fn meld_ability_from(
    source_id: ObjectId,
    controller: PlayerId,
    source: &str,
    partner: &str,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Meld {
            source: source.to_string(),
            partner: partner.to_string(),
            result: RESULT_NAME.to_string(),
            source_filter: crate::types::ability::TargetFilter::SelfRef,
            partner_filter: crate::types::ability::TargetFilter::Any,
            entry: crate::types::ability::PermanentEntryMode::Normal,
        },
        Vec::new(),
        source_id,
        controller,
    )
}

/// Two co-owned/controlled meld halves on P0's battlefield, plus a seeded result
/// face. Returns `(state, source_id, partner_id)`.
fn both_halves() -> (crate::types::game_state::GameState, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, "Gisela, the Broken Blade", 4, 3).id();
    let partner = sc.add_creature(P0, "Bruna, the Fading Light", 5, 4).id();
    seed_result_face(&mut sc.state);
    (sc.state, source, partner)
}

/// CR 701.42a / CR 712.4a: melding exiles both halves and puts a SINGLE melded
/// permanent onto the battlefield presenting the RESULT card's characteristics.
#[test]
fn meld_exiles_both_produces_single_permanent() {
    let (mut state, source, partner) = both_halves();
    let mut events = Vec::new();
    let ability = meld_ability(source, P0, "Bruna, the Fading Light");

    perform_meld(&mut state, &ability, &mut events).unwrap();

    // The survivor (source) is on the battlefield; the partner is no longer an
    // independent battlefield object.
    let survivor = state.objects.get(&source).expect("survivor exists");
    assert_eq!(survivor.zone, Zone::Battlefield);
    assert_eq!(
        survivor.merged_components,
        vec![source, partner],
        "the melded permanent records both halves"
    );
    assert!(
        !state.battlefield.iter().any(|&id| id == partner),
        "the partner half is absorbed into the melded permanent"
    );

    // CR 701.42a / CR 730.2: the partner is absorbed — it is NOT an independent
    // object in the exile list, yet its `zone` reads Battlefield (a component in
    // no zone list, mirroring merge_object_onto). On the pre-fix code the partner
    // was stranded in the exile list with zone == Exile, so all three of these
    // assertions fail without the absorption fix.
    let partner_obj = state.objects.get(&partner).expect("partner exists");
    assert_eq!(
        partner_obj.zone,
        Zone::Battlefield,
        "the absorbed partner's zone is Battlefield (component, not stranded in Exile)"
    );
    assert!(
        !state.exile.iter().any(|&id| id == partner),
        "the absorbed partner is NOT left in the exile zone list"
    );
    assert!(
        !state.battlefield.iter().any(|&id| id == partner),
        "the absorbed partner is a component, not an independent battlefield object"
    );

    // CR 712.4b: the melded permanent presents the RESULT card's characteristics
    // (Brisela 9/10) through the installed layer-1 copy effect.
    assert_eq!(survivor.name, RESULT_NAME);
    assert_eq!(survivor.power, Some(9));
    assert_eq!(survivor.toughness, Some(10));

    // CR 712.4b / CR 712.21: the survivor's BASE identity is NOT corrupted — it
    // is still its own front face (Gisela), so it returns correctly on leave.
    assert_eq!(survivor.base_name, "Gisela, the Broken Blade");
}

/// CR 712.21 / CR 712.4b: when the melded permanent leaves the battlefield, the
/// two cards return as their OWN FRONT FACES, each to its owner's graveyard.
#[test]
fn leave_split_returns_front_faces() {
    let (mut state, source, partner) = both_halves();
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    // Destroy the melded permanent (battlefield → graveyard).
    let mut leave_events = Vec::new();
    crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut leave_events);

    let survivor = state
        .objects
        .get(&source)
        .expect("survivor object persists");
    assert_eq!(survivor.zone, Zone::Graveyard);
    // CR 712.4b: returns as its own front face, NOT as Brisela.
    assert_eq!(survivor.name, "Gisela, the Broken Blade");
    assert!(
        survivor.merged_components.is_empty(),
        "merge identity cleared on exit"
    );
    assert!(
        survivor.merge_kind.is_none(),
        "meld discriminator cleared on exit"
    );

    // CR 712.21: the partner card returns as its own front face, to its owner.
    let partner_obj = state.objects.get(&partner).expect("partner card returns");
    assert_eq!(partner_obj.zone, Zone::Graveyard);
    assert_eq!(partner_obj.name, "Bruna, the Fading Light");
    assert_eq!(partner_obj.owner, P0);

    // CR 701.42a / CR 730.2: the partner is single-listed in the graveyard and is
    // NOT double-listed in exile. On the pre-fix code the partner was stranded in
    // the exile list at meld time, so after the leave-split it remained in exile
    // AND was added to the graveyard — these two assertions catch that corruption.
    let p0_graveyard = &state
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .graveyard;
    assert!(
        p0_graveyard.iter().any(|&id| id == partner),
        "the partner is listed in its owner's graveyard exactly once"
    );
    assert!(
        !state.exile.iter().any(|&id| id == partner),
        "the partner is NOT double-listed in exile after the leave-split"
    );
}

/// CR 701.42c: if the partner is absent (or not co-owned/controlled), the meld is
/// a no-op — the instigator stays on the battlefield, nothing is exiled.
#[test]
fn intervening_if_gates_both_ways() {
    // Partner ABSENT: only the source is on the battlefield.
    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, "Gisela, the Broken Blade", 4, 3).id();
    seed_result_face(&mut sc.state);
    let mut state = sc.state;
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    let src = state.objects.get(&source).expect("source persists");
    assert_eq!(src.zone, Zone::Battlefield, "no-op: source stays put");
    assert!(src.merged_components.is_empty(), "no meld occurred");

    // Partner PRESENT but owned by a DIFFERENT player (controlled by P0 but not
    // owned) → still a no-op (CR 701.42b own AND control).
    let (mut state, source, _partner) = both_halves();
    // Re-own the partner to P1 while leaving control with P0.
    let partner2 = state
        .objects
        .iter()
        .find(|(_, o)| o.name == "Bruna, the Fading Light")
        .map(|(id, _)| *id)
        .unwrap();
    state.objects.get_mut(&partner2).unwrap().owner = P1;
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();
    assert!(
        state
            .objects
            .get(&source)
            .unwrap()
            .merged_components
            .is_empty(),
        "CR 701.42b: a partner you control but don't own can't be melded"
    );
}

/// CR 712.4c: a melded permanent cannot be transformed — the instruction is a
/// silent no-op, and the permanent keeps presenting the result + its merge state.
#[test]
fn meld_permanent_cannot_transform() {
    let (mut state, source, _partner) = both_halves();
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    // Attempt to transform the melded permanent — silent no-op (CR 712.4c).
    let mut t_events = Vec::new();
    crate::game::transform::transform_permanent(&mut state, source, &mut t_events).unwrap();

    let survivor = state.objects.get(&source).expect("survivor persists");
    assert_eq!(survivor.name, RESULT_NAME, "still presents the result");
    assert_eq!(
        survivor.merged_components,
        vec![source, _partner],
        "merge state intact after the ignored transform"
    );
}

/// CR 603.6a / CR 701.42a: melding emits a battlefield-entry `ZoneChanged` event
/// for the survivor (unlike Mutate, which suppresses ETB per CR 730.2b), so ETB
/// triggers can match the entering melded permanent.
#[test]
fn etb_fires_on_meld() {
    let (mut state, source, _partner) = both_halves();
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == source
        )),
        "the melded permanent's entry emits a battlefield ZoneChanged so ETB can fire"
    );
}

// ---------------------------------------------------------------------------
// Hardening tests (PR #3023): printed-identity legality gate + pipeline entry.
//
// Tests `meld_token_partner_is_noop` and `meld_renamed_non_meld_partner_is_noop`
// are DISCRIMINATING — they FAIL on the pre-fix resolver (the old
// `FilterProp::Named` finder matched the layer-modified `name` and did not gate
// on card-backing, so a token/copy/renamed impostor was melded) and PASS only
// with the `base_name` + `is_represented_by_a_card()` gate. Test
// `meld_entry_consults_enters_with_replacement` is the entry-seam discriminator:
// the raw `move_to_zone` skipped the entry replacement consult, so the survivor
// did not enter tapped; routing through `zone_pipeline::move_object` runs the
// consult (CR 614.1c / CR 614.12a).
// ---------------------------------------------------------------------------

/// CR 701.42a / CR 712.4a (production-shaped): real Gisela + Bruna loaded from
/// the card database meld into a SINGLE Brisela permanent. Drives the real
/// resolver against real parsed card faces (`add_real_card`), seeding the result
/// face the same way production does. SKIPped if `card-data.json` is absent.
#[test]
fn meld_production_shaped_real_cards_single_permanent() {
    use crate::game::scenario_db::GameScenarioDbExt;

    let db = crate::test_support::shared_card_db();

    let mut sc = GameScenario::new();
    let source = sc.add_real_card(P0, "Gisela, the Broken Blade", Zone::Battlefield, db);
    let partner = sc.add_real_card(P0, "Bruna, the Fading Light", Zone::Battlefield, db);
    let mut state = sc.state;
    // `add_real_card` does NOT seed `card_face_registry`; `perform_meld` no-ops
    // without the Brisela result face, so seed it explicitly.
    seed_result_face(&mut state);

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    let survivor = state.objects.get(&source).expect("survivor exists");
    assert_eq!(survivor.zone, Zone::Battlefield);
    assert_eq!(
        survivor.merged_components,
        vec![source, partner],
        "the melded permanent records both real halves"
    );
    // The partner is absorbed: not an independent battlefield object, not in
    // exile, but its zone reads Battlefield (a component, in no zone list).
    assert!(
        !state.battlefield.iter().any(|&id| id == partner),
        "the partner half is absorbed, not an independent battlefield object"
    );
    assert!(
        !state.exile.iter().any(|&id| id == partner),
        "the partner is not stranded in exile"
    );
    assert_eq!(
        state.objects.get(&partner).expect("partner exists").zone,
        Zone::Battlefield,
        "the absorbed partner's zone is Battlefield"
    );
    // CR 712.4b: presents the result identity; base identity (Gisela front face)
    // is intact for the leave-split.
    assert_eq!(survivor.name, RESULT_NAME);
    assert_eq!(survivor.base_name, "Gisela, the Broken Blade");
}

/// CR 701.42b (CR 111.1): a TOKEN copy named like a meld half is NOT a real meld
/// card and cannot be melded. The live name condition is checked before the
/// exile instruction; physical-card validation happens afterward, so both
/// selected objects remain in exile without becoming a melded permanent.
#[test]
fn meld_token_partner_is_noop() {
    let (mut state, source, partner) = both_halves();
    state.objects.get_mut(&partner).unwrap().is_token = true;
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    let src = state.objects.get(&source).expect("source persists");
    assert_eq!(src.zone, Zone::Exile);
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred with a token partner"
    );
    assert!(
        state.exile.contains(&source) && state.exile.contains(&partner),
        "CR 701.42: the instruction exiles the selected live referents before physical validation"
    );
}

/// CR 701.42b (CR 707.10): a COPY named like a meld half cannot be melded.
#[test]
fn meld_copy_partner_is_noop() {
    let (mut state, source, partner) = both_halves();
    state.objects.get_mut(&partner).unwrap().is_copy = true;
    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    let src = state.objects.get(&source).expect("source persists");
    assert_eq!(src.zone, Zone::Exile);
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred with a copy partner"
    );
    assert!(
        state.exile.contains(&source) && state.exile.contains(&partner),
        "the selected copy is rejected only after both objects are exiled"
    );
}

/// CR 701.42b: a card-backed NON-MELD permanent renamed (via a continuous effect)
/// to the partner's name is an IMPOSTOR — its PRINTED identity (`base_name`) is
/// not the meld half, so it cannot be melded. DISCRIMINATING: the pre-fix finder
/// matched the layer-modified current `name`, so the impostor WOULD have been
/// melded; matching `base_name` rejects it.
#[test]
fn meld_renamed_non_meld_partner_is_noop() {
    use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
    use crate::types::game_state::TransientContinuousEffect;

    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, "Gisela, the Broken Blade", 4, 3).id();
    // A vanilla, card-backed creature with its OWN printed identity.
    let impostor = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut state = sc.state;
    seed_result_face(&mut state);

    // Install a continuous effect renaming the impostor's current `name` to the
    // partner's name (CR 613 layer 7-equivalent SetName; layer pass overwrites
    // `name` but never `base_name`). `Duration::Permanent` stays live through
    // `flush_layers` (no turn passes in this test).
    let ts = state.next_timestamp();
    state
        .transient_continuous_effects
        .push_back(TransientContinuousEffect {
            id: 1,
            source_id: impostor,
            controller: P0,
            timestamp: ts,
            duration: Duration::Permanent,
            affected: TargetFilter::SelfRef,
            modifications: vec![ContinuousModification::SetName {
                name: "Bruna, the Fading Light".to_string(),
            }],
            condition: None,
            duration_subject: None,
            source_name: String::new(),
        });
    crate::game::layers::flush_layers(&mut state);

    // Precondition: the impostor presents the partner's NAME but keeps its own
    // printed identity (base_name).
    let imp = state.objects.get(&impostor).expect("impostor exists");
    assert_eq!(
        imp.name, "Bruna, the Fading Light",
        "impostor renamed by effect"
    );
    assert_eq!(imp.base_name, "Grizzly Bears", "printed identity unchanged");

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    // The live renamed object satisfies the instruction and is exiled; its
    // physical identity then fails the post-exile meld validation.
    let src = state.objects.get(&source).expect("source persists");
    assert_eq!(src.zone, Zone::Exile);
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred against a renamed impostor"
    );
    let imp = state.objects.get(&impostor).expect("impostor persists");
    assert_eq!(
        imp.zone,
        Zone::Exile,
        "the live named referent is exiled before its physical identity is checked"
    );
    assert!(
        state.exile.contains(&source) && state.exile.contains(&impostor),
        "both selected objects remain exiled when they cannot physically meld"
    );
}

/// CR 701.42b: a card-backed NON-MELD source cannot be used as the meld
/// instigator. The resolver must check the source's printed identity too, not
/// only the partner's identity.
#[test]
fn meld_non_meld_source_is_noop() {
    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let partner = sc.add_creature(P0, "Bruna, the Fading Light", 5, 4).id();
    seed_result_face(&mut sc.state);
    let mut state = sc.state;

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability_from(
            source,
            P0,
            "Gisela, the Broken Blade",
            "Bruna, the Fading Light",
        ),
        &mut events,
    )
    .unwrap();

    let src = state.objects.get(&source).expect("source persists");
    assert_eq!(src.zone, Zone::Exile);
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred with a non-meld source"
    );
    assert_eq!(
        state.objects.get(&partner).expect("partner persists").zone,
        Zone::Exile,
        "the live partner is exiled before the source's physical identity fails validation"
    );
    assert!(
        state.exile.contains(&source) && state.exile.contains(&partner),
        "both selected objects remain exiled after physical validation fails"
    );
}

fn install_exile_redirect(
    state: &mut crate::types::game_state::GameState,
    name: Option<&str>,
    destination: Zone,
    optional: bool,
) {
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, FilterProp, ReplacementDefinition, ReplacementMode,
        TargetFilter, TypedFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;

    let redirector = create_object(
        state,
        CardId(state.next_object_id),
        P1,
        format!("Exile Redirector to {destination:?}"),
        Zone::Battlefield,
    );
    let target = name.map_or(TargetFilter::Any, |name| {
        TargetFilter::Typed(TypedFilter {
            type_filters: Vec::new(),
            controller: None,
            properties: vec![FilterProp::Named {
                name: name.to_string(),
            }],
        })
    });
    let mut redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(target)
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ));
    if optional {
        redirect = redirect.mode(ReplacementMode::Optional { decline: None });
    }
    state
        .objects
        .get_mut(&redirector)
        .unwrap()
        .replacement_definitions
        .push(redirect);
}

fn assert_redirected_exile_pair_melds(redirects: &[(&str, Zone)]) {
    let (mut state, source, partner) = both_halves();
    for (name, destination) in redirects {
        install_exile_redirect(&mut state, Some(name), *destination, false);
    }

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    assert_eq!(state.objects[&source].zone, Zone::Battlefield);
    assert_eq!(state.objects[&source].name, RESULT_NAME);
    assert_eq!(
        state.objects[&source].merged_components,
        vec![source, partner]
    );
    for (name, destination) in redirects {
        if *destination == Zone::Battlefield {
            assert!(!events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. }
                    if state.objects[object_id].base_name.eq_ignore_ascii_case(name)
            )));
        } else {
            assert!(events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, to, .. }
                    if state.objects[object_id].base_name.eq_ignore_ascii_case(name)
                        && *to == *destination
            )));
        }
    }
}

/// CR 400.7j + CR 701.42b: replacing the first exile attempt with the card
/// remaining on the battlefield does not lose the tracked physical referent.
#[test]
fn meld_first_exile_prevented_by_battlefield_redirect_still_melds() {
    assert_redirected_exile_pair_melds(&[("Gisela, the Broken Blade", Zone::Battlefield)]);
}

/// CR 400.2 + CR 400.7j + CR 701.42b: redirecting the second selected card's
/// exile move to another public zone still leaves that physical card findable.
#[test]
fn meld_second_exile_redirected_to_graveyard_still_melds() {
    assert_redirected_exile_pair_melds(&[("Bruna, the Fading Light", Zone::Graveyard)]);
}

/// CR 400.2 + CR 400.7j + CR 701.42b: independent redirects of both exile
/// attempts to public zones still allow exactly those two tracked cards to meld.
#[test]
fn meld_both_exiles_redirected_to_public_zones_still_melds() {
    assert_redirected_exile_pair_melds(&[
        ("Gisela, the Broken Blade", Zone::Graveyard),
        ("Bruna, the Fading Light", Zone::Command),
    ]);
}

/// CR 400.2 + CR 400.7j + CR 701.42c: a redirect into a hidden zone makes that
/// card unavailable to the later meld instruction, so both selected cards stay
/// in the zones produced by the exile instruction.
#[test]
fn meld_hidden_zone_redirect_is_not_findable_and_preserves_current_zones() {
    let (mut state, source, partner) = both_halves();
    install_exile_redirect(
        &mut state,
        Some("Gisela, the Broken Blade"),
        Zone::Hand,
        false,
    );

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    assert_eq!(state.objects[&source].zone, Zone::Hand);
    assert_eq!(state.objects[&partner].zone, Zone::Exile);
    assert!(state.objects[&source].merged_components.is_empty());
    assert!(state.exile.contains(&partner));
}

/// CR 616.1 + CR 400.7j + CR 701.42b: when each exile attempt independently
/// pauses for an optional public-zone redirect, the parked simultaneous batch
/// preserves the selected IDs and runs physical-pair validation only after the
/// second pause resolves.
#[test]
fn meld_exile_batch_survives_repeated_replacement_pauses() {
    use crate::game::engine::apply_as_current;
    use crate::types::actions::GameAction;

    let (mut state, source, partner) = both_halves();
    install_exile_redirect(&mut state, None, Zone::Graveyard, true);

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(state.objects[&source].zone, Zone::Battlefield);
    assert_eq!(state.objects[&partner].zone, Zone::Battlefield);

    apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
        .expect("accept the first exile redirect");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(state.objects[&source].zone, Zone::Graveyard);
    assert_eq!(state.objects[&partner].zone, Zone::Battlefield);

    let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
        .expect("accept the second exile redirect");
    assert_eq!(state.objects[&source].zone, Zone::Battlefield);
    assert_eq!(state.objects[&source].name, RESULT_NAME);
    assert_eq!(
        state.objects[&source].merged_components,
        vec![source, partner]
    );
    assert_eq!(
        events
            .iter()
            .chain(result.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::Meld,
                    ..
                }
            ))
            .count(),
        1,
        "the twice-paused exile batch must complete the meld exactly once"
    );
}

/// CR 614.1c / CR 614.12a: the survivor's exile→battlefield entry is routed
/// through the zone-change pipeline using the result card's projected
/// characteristics, so its intrinsic enters-tapped replacement is consulted.
/// DISCRIMINATING: the pre-fix raw
/// `move_to_zone` skipped the entry consult, so the survivor would NOT enter
/// tapped; the pipeline runs the consult.
#[test]
fn meld_entry_consults_enters_with_replacement() {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect as AbilEffect, EffectScope, ReplacementDefinition,
        TapStateChange, TargetFilter,
    };
    use crate::types::replacements::ReplacementEvent;

    let (mut state, source, _partner) = both_halves();

    // A self-scoped "enters tapped" replacement on the result face (CR 614.1c /
    // CR 614.12a): the replacement's execute is the canonical SelfRef single
    // `SetTapState { Tap }` that `event_modifiers_for_ability` reads as the
    // enters-tapped modifier (CR 701.26a). Its exile→battlefield entry is the
    // ChangeZone event the consult must replace.
    let enters_tapped = ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            AbilEffect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        ))
        .description("This permanent enters the battlefield tapped.".to_string());
    let result_face = Arc::make_mut(&mut state.card_face_registry)
        .get_mut(&RESULT_NAME.to_lowercase())
        .expect("seeded result face");
    result_face.replacements.push(enters_tapped);

    // Precondition: the survivor is currently untapped.
    assert!(
        !state.objects.get(&source).unwrap().tapped,
        "precondition: survivor untapped before meld"
    );

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    let survivor = state.objects.get(&source).expect("survivor persists");
    assert_eq!(survivor.zone, Zone::Battlefield);
    assert!(
        survivor.tapped,
        "the entry consult ran: the survivor entered tapped (raw move_to_zone would skip it)"
    );
    assert_eq!(
        survivor.merged_components,
        vec![source, _partner],
        "the meld still produced the merged permanent through the pipeline entry"
    );
}

/// Drive a meld whose battlefield entry is redirected to hand while both
/// physical components are commanders, then make the requested independent
/// commander-zone decisions for the source and partner.
fn run_redirected_meld_commander_choices(source_accepts: bool) {
    use std::collections::HashSet;

    use crate::game::scenario::GameRunner;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect as AbilEffect, FilterProp, ReplacementDefinition,
        TargetFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::replacements::ReplacementEvent;

    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, "Gisela, the Broken Blade", 4, 3).id();
    let partner = sc.add_creature(P0, "Bruna, the Fading Light", 5, 4).id();
    let redirector = sc.add_creature(P1, "Entry Redirector", 2, 2).id();
    seed_result_face(&mut sc.state);
    sc.state.format_config.command_zone = true;
    sc.state.objects.get_mut(&source).unwrap().is_commander = true;
    sc.state.objects.get_mut(&partner).unwrap().is_commander = true;

    // CR 614.5: the same replacement effect gets only one opportunity to
    // modify the result-entry event. The partner delivery inherits that
    // applied-effect set, so this battlefield-to-hand redirect cannot loop or
    // pause between the two physical components.
    let redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::Any)
        .destination_zone(Zone::Battlefield)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            AbilEffect::ChangeZone {
                destination: Zone::Hand,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description("If a permanent would enter, put it into its owner's hand instead.".into());
    sc.state
        .objects
        .get_mut(&redirector)
        .unwrap()
        .replacement_definitions
        .push(redirect);

    // CR 400.6 + CR 614.5: the physical partner's derived Exile→Hand move is a
    // separately replaceable event, seeded with the result redirect's applied
    // set. The original Battlefield→Hand redirect cannot recur, while this
    // partner-specific Hand→Graveyard replacement still gets one opportunity.
    let split_partner = ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::Typed(TypedFilter::default().properties(
            vec![FilterProp::Named {
                name: "Bruna, the Fading Light".to_string(),
            }],
        )))
        .destination_zone(Zone::Hand)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            AbilEffect::ChangeZone {
                destination: Zone::Graveyard,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description(
            "If Bruna would move to hand, put it into its owner's graveyard instead.".into(),
        );
    sc.state
        .objects
        .get_mut(&redirector)
        .unwrap()
        .replacement_definitions
        .push(split_partner);

    let mut meld_events = Vec::new();
    perform_meld(
        &mut sc.state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut meld_events,
    )
    .unwrap();

    assert_eq!(sc.state.objects[&source].zone, Zone::Hand);
    assert_eq!(sc.state.objects[&partner].zone, Zone::Graveyard);
    assert_eq!(
        meld_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::Meld,
                    ..
                }
            ))
            .count(),
        1,
        "the redirected meld finishes exactly once after both component deliveries"
    );

    // CR 903.9a / CR 903.9b: each commander component receives its own choice
    // from its independently replaced destination. Choice order is engine-
    // defined, so decide by component identity while proving neither is
    // prompted twice and the second prompt follows the first decision.
    crate::game::sba::check_state_based_actions(&mut sc.state, &mut meld_events);
    let mut runner = GameRunner::from_state(sc.state);
    let mut prompted = HashSet::new();
    let mut choice_events = Vec::new();
    for _ in 0..2 {
        let WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone,
            ..
        } = runner.state().waiting_for
        else {
            panic!(
                "expected one commander prompt per meld component, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            current_zone,
            if commander_id == source {
                Zone::Hand
            } else {
                Zone::Graveyard
            }
        );
        assert!(
            prompted.insert(commander_id),
            "a meld component must not receive the commander choice twice"
        );
        let accept = if commander_id == source {
            source_accepts
        } else {
            assert_eq!(commander_id, partner);
            !source_accepts
        };
        let result = runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("commander-zone decision resolves");
        choice_events.extend(result.events);
    }

    assert_eq!(prompted, HashSet::from([source, partner]));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        choice_events.iter().all(|event| !matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Meld,
                ..
            }
        )),
        "commander choices must not resume the already-finished meld"
    );

    let expected_source_zone = if source_accepts {
        Zone::Command
    } else {
        Zone::Hand
    };
    let expected_partner_zone = if source_accepts {
        Zone::Graveyard
    } else {
        Zone::Command
    };
    assert_eq!(runner.state().objects[&source].zone, expected_source_zone);
    assert_eq!(runner.state().objects[&partner].zone, expected_partner_zone);
}

#[test]
fn redirected_meld_source_commander_accepts_partner_declines() {
    run_redirected_meld_commander_choices(true);
}

#[test]
fn redirected_meld_source_commander_declines_partner_accepts() {
    run_redirected_meld_commander_choices(false);
}

/// CR 614.12 + CR 701.42: while an as-enters replacement choice is pending,
/// the meld result exists only as a private liminal projection. Both physical
/// cards remain unchanged public exile objects until the entry commits.
#[test]
fn meld_replacement_pause_keeps_result_projection_detached() {
    use crate::types::ability::{ReplacementDefinition, ReplacementMode, TargetFilter};
    use crate::types::replacements::ReplacementEvent;

    let (mut state, source, partner) = both_halves();
    let optional_entry = ReplacementDefinition::new(ReplacementEvent::Moved)
        .mode(ReplacementMode::Optional { decline: None })
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description("You may apply this result entry replacement.".to_string());
    Arc::make_mut(&mut state.card_face_registry)
        .get_mut(&RESULT_NAME.to_lowercase())
        .expect("seeded result face")
        .replacements
        .push(optional_entry);

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &meld_ability(source, P0, "Bruna, the Fading Light"),
        &mut events,
    )
    .unwrap();

    assert!(matches!(
        state.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(state.objects[&source].zone, Zone::Exile);
    assert_eq!(state.objects[&partner].zone, Zone::Exile);
    assert_eq!(state.objects[&source].name, "Gisela, the Broken Blade");
    assert_eq!(state.objects[&partner].name, "Bruna, the Fading Light");
    assert_eq!(
        state.liminal_entries[&source].object.name, RESULT_NAME,
        "replacement matching sees the detached result projection"
    );

    let public = crate::game::visibility::filter_state_for_viewer(&state, P0);
    assert!(public.liminal_entries.is_empty());
    assert_eq!(public.objects[&source].name, "Gisela, the Broken Blade");
    assert_eq!(public.objects[&partner].name, "Bruna, the Fading Light");
}

/// CR 306.5b + CR 614.12 + CR 701.42: a planeswalker meld result seeds its
/// intrinsic loyalty counters from the projected result face, not the exiled
/// source component's creature face.
#[test]
fn meld_result_seeds_intrinsic_loyalty_counters() {
    use crate::types::counter::CounterType;

    const SOURCE: &str = "Urza, Lord Protector";
    const PARTNER: &str = "The Mightstone and Weakstone";
    const RESULT: &str = "Urza, Planeswalker";

    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, SOURCE, 2, 4).id();
    let partner = sc.add_creature(P0, PARTNER, 0, 0).as_artifact().id();
    let mut result_face = CardFace {
        name: RESULT.to_string(),
        loyalty: Some("7".to_string()),
        ..CardFace::default()
    };
    result_face
        .card_type
        .core_types
        .push(CoreType::Planeswalker);
    Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), result_face);
    seed_meld_pair(&mut sc.state, SOURCE, PARTNER, RESULT);
    let ability = ResolvedAbility::new(
        Effect::Meld {
            source: SOURCE.to_string(),
            partner: PARTNER.to_string(),
            result: RESULT.to_string(),
            source_filter: crate::types::ability::TargetFilter::SelfRef,
            partner_filter: crate::types::ability::TargetFilter::Any,
            entry: crate::types::ability::PermanentEntryMode::Normal,
        },
        Vec::new(),
        source,
        P0,
    );

    let mut events = Vec::new();
    perform_meld(&mut sc.state, &ability, &mut events).unwrap();

    let result = &sc.state.objects[&source];
    assert_eq!(result.zone, Zone::Battlefield);
    assert_eq!(result.name, RESULT);
    assert_eq!(result.counters.get(&CounterType::Loyalty), Some(&7));
    assert_eq!(result.loyalty, Some(7));
    assert_eq!(result.merged_components, vec![source, partner]);
}

/// CR 608.2c + CR 701.42a: Hanweir and Urza's production activated ability
/// definitions retain their inline own/control condition through the ordinary
/// resolved-ability chain and meld the two qualifying physical cards.
#[test]
fn production_activated_inline_gates_resolve_hanweir_and_urza() {
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::effects::resolve_ability_chain;

    const HANWEIR_TEXT: &str = "{T}: Add {R}.\n\
        {3}{R}{R}, {T}: If you both own and control this land and a creature named Hanweir \
        Garrison, exile them, then meld them into Hanweir, the Writhing Township. Activate only \
        as a sorcery.";
    const URZA_TEXT: &str = "Artifact, instant, and sorcery spells you cast cost {1} less to \
        cast.\n{7}: If you both own and control Urza, Lord Protector and an artifact named The \
        Mightstone and Weakstone, exile them, then meld them into Urza, Planeswalker. Activate \
        only as a sorcery.";

    fn contains_meld(def: &crate::types::ability::AbilityDefinition) -> bool {
        matches!(def.effect.as_ref(), Effect::Meld { .. })
            || def.sub_ability.as_deref().is_some_and(contains_meld)
            || def.else_ability.as_deref().is_some_and(contains_meld)
            || def.mode_abilities.iter().any(contains_meld)
    }

    fn activated_meld(
        state: &crate::types::game_state::GameState,
        source: ObjectId,
    ) -> crate::types::ability::AbilityDefinition {
        state.objects[&source]
            .abilities
            .iter()
            .find(|ability| contains_meld(ability))
            .cloned()
            .expect("production Oracle text exposes an activated Meld ability")
    }

    {
        const SOURCE: &str = "Hanweir Battlements";
        const PARTNER: &str = "Hanweir Garrison";
        const RESULT: &str = "Hanweir, the Writhing Township";

        let mut sc = GameScenario::new();
        let source = sc.add_land_from_oracle(P0, SOURCE, HANWEIR_TEXT).id();
        let partner = sc.add_creature(P0, PARTNER, 2, 3).id();
        let mut result_face = CardFace {
            name: RESULT.to_string(),
            power: Some(PtValue::Fixed(7)),
            toughness: Some(PtValue::Fixed(4)),
            ..CardFace::default()
        };
        result_face.card_type.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), result_face);
        seed_meld_pair(&mut sc.state, SOURCE, PARTNER, RESULT);

        let ability = activated_meld(&sc.state, source);
        let resolved = build_resolved_from_def(&ability, source, P0);
        let mut events = Vec::new();
        resolve_ability_chain(&mut sc.state, &resolved, &mut events, 0)
            .expect("Hanweir's activated Meld ability resolves");

        let survivor = &sc.state.objects[&source];
        assert_eq!(survivor.name, RESULT);
        assert_eq!(survivor.merged_components, vec![source, partner]);
    }

    {
        const SOURCE: &str = "Urza, Lord Protector";
        const PARTNER: &str = "The Mightstone and Weakstone";
        const RESULT: &str = "Urza, Planeswalker";

        let mut sc = GameScenario::new();
        let source = sc
            .add_creature_from_oracle(P0, SOURCE, 2, 4, URZA_TEXT)
            .id();
        let partner = sc.add_creature(P0, PARTNER, 0, 0).as_artifact().id();
        let mut result_face = CardFace {
            name: RESULT.to_string(),
            loyalty: Some("7".to_string()),
            ..CardFace::default()
        };
        result_face
            .card_type
            .core_types
            .push(CoreType::Planeswalker);
        Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), result_face);
        seed_meld_pair(&mut sc.state, SOURCE, PARTNER, RESULT);

        let ability = activated_meld(&sc.state, source);
        let resolved = build_resolved_from_def(&ability, source, P0);
        let mut events = Vec::new();
        resolve_ability_chain(&mut sc.state, &resolved, &mut events, 0)
            .expect("Urza's activated Meld ability resolves");

        let survivor = &sc.state.objects[&source];
        assert_eq!(survivor.name, RESULT);
        assert_eq!(survivor.merged_components, vec![source, partner]);
    }
}

/// CR 201.2a + CR 201.5c: end-to-end proof that the FIX-3 self-ref mask makes a
/// shared-token meld RESULT resolvable at runtime. Drives the real parser
/// (`parse_oracle_text`) → resolver (`perform_meld`) seam for Titania, whose
/// result "Titania, Gaea Incarnate" shares its pre-comma legendary token with the
/// instigator. The registry is seeded under the TRUE result key; the ResolvedAbility
/// carries the string the parser actually emitted. Reverting the mask makes the
/// parser emit "~, Gaea Incarnate", which (a) trips the string assertion and (b)
/// misses the seeded face so `perform_meld` is a silent no-op — the merge
/// assertions then fail. This is the "green-but-dead" guard the mask exists for.
#[test]
fn parsed_meld_result_name_resolves_through_registry() {
    const TITANIA: &str = "Reach\nWhenever one or more land cards are put into your graveyard \
         from anywhere, you gain 2 life.\nAt the beginning of your upkeep, if there are four or \
         more land cards in your graveyard and you both own and control Titania, Voice of Gaea \
         and a land named Argoth, Sanctum of Nature, exile them, then meld them into Titania, \
         Gaea Incarnate.";
    const RESULT: &str = "Titania, Gaea Incarnate";

    // Extract the meld RESULT string the production parser emits.
    fn find_meld(
        def: &crate::types::ability::AbilityDefinition,
    ) -> Option<(String, String, String)> {
        if let Effect::Meld {
            source,
            partner,
            result,
            ..
        } = def.effect.as_ref()
        {
            return Some((source.clone(), partner.clone(), result.clone()));
        }
        def.sub_ability
            .as_deref()
            .and_then(find_meld)
            .or_else(|| def.else_ability.as_deref().and_then(find_meld))
            .or_else(|| def.mode_abilities.iter().find_map(find_meld))
    }
    let parsed = crate::parser::oracle::parse_oracle_text(
        TITANIA,
        "Titania, Voice of Gaea",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let (src_name, partner_name, result_name) = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(find_meld)
        .expect("Titania lowers to an Effect::Meld");
    assert_eq!(
        result_name, RESULT,
        "mask keeps the shared-token result verbatim"
    );

    // Battlefield: both co-owned/controlled halves under P0.
    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, &src_name, 3, 4).id();
    let partner = sc.add_creature(P0, &partner_name, 1, 1).id();
    // Seed the registry under the TRUE result-card key (what card-data registers),
    // NOT under the parsed string — so a corrupted "~, …" cannot coincidentally hit.
    let mut face = CardFace {
        name: RESULT.to_string(),
        power: Some(PtValue::Fixed(0)),
        toughness: Some(PtValue::Fixed(0)),
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), face);
    seed_meld_pair(&mut sc.state, &src_name, &partner_name, RESULT);
    let mut state = sc.state;

    let ability = ResolvedAbility::new(
        Effect::Meld {
            source: src_name,
            partner: partner_name,
            result: result_name,
            source_filter: crate::types::ability::TargetFilter::SelfRef,
            partner_filter: crate::types::ability::TargetFilter::Any,
            entry: crate::types::ability::PermanentEntryMode::Normal,
        },
        Vec::new(),
        source,
        P0,
    );
    let mut events = Vec::new();
    perform_meld(&mut state, &ability, &mut events).unwrap();

    let survivor = state.objects.get(&source).expect("survivor persists");
    assert_eq!(
        survivor.merged_components,
        vec![source, partner],
        "the parser's result string resolved into a melded permanent"
    );
    assert_eq!(
        survivor.name, RESULT,
        "survivor presents Titania, Gaea Incarnate"
    );
}

/// CR 118.12 + CR 608.2c (end-to-end): Vanille's meld trigger lowers to an
/// OPTIONAL `PayCost {3}{B}{G}` whose `Effect::Meld` sub-ability is gated on
/// `OptionalEffectPerformed`. Drives the FULL production seam — parse the real
/// Oracle text, build the trigger's execute, resolve it through
/// `resolve_ability_chain` (depth 0, exactly as the engine resolves a stack
/// trigger), then submit the real `GameAction::DecideOptionalEffect` through
/// `GameRunner::act` — for BOTH branches:
///
///   * accept  ⇒ the {3}{B}{G} pay is performed, so the gated meld fires: Vanille
///     + Fang meld into Ragnarok, and the pool is drained.
///   * decline ⇒ the pay is NOT performed, so `OptionalEffectPerformed` is false
///     and the meld does NOT fire (CR 118.12: declining a resolution cost must not
///     perform the gated effect). Both halves remain; no mana is spent.
///
/// Revert discriminators: reverting the `parse_meld_gate` / per-clause dispatch
/// (Meld → Unimplemented) makes the ACCEPT branch fail to meld; reverting the
/// `OptionalEffectPerformed` sub-gate makes the DECLINE branch meld anyway,
/// failing the decline assertions. Non-vacuity: each branch first asserts the
/// `OptionalEffectChoice` pay prompt is actually reached before the decision.
#[test]
fn vanille_optional_pay_gates_meld_accept_vs_decline() {
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::scenario::GameRunner;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::AbilityDefinition;
    use crate::types::actions::GameAction;
    use crate::types::game_state::WaitingFor;
    use crate::types::mana::{ManaType, ManaUnit};

    // Verbatim Scryfall Oracle text (checked 2026-07 via api.scryfall.com).
    const VANILLE: &str = "When Vanille enters, mill two cards, then return a permanent card \
         from your graveyard to your hand.\nAt the beginning of your first main phase, if you \
         both own and control Vanille and a creature named Fang, Fearless l'Cie, you may pay \
         {3}{B}{G}. If you do, exile them, then meld them into Ragnarok, Divine Deliverance.";
    const RESULT: &str = "Ragnarok, Divine Deliverance";

    // The meld-bearing trigger's execute: a `PayCost` with a `Meld` sub-ability.
    fn vanille_meld_execute() -> AbilityDefinition {
        let parsed = parse_oracle_text(
            VANILLE,
            "Vanille, Cheerful l'Cie",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &[],
        );
        parsed
            .triggers
            .into_iter()
            .find_map(|t| {
                let execute = t.execute?;
                let is_meld_pay = matches!(execute.effect.as_ref(), Effect::PayCost { .. })
                    && execute
                        .sub_ability
                        .as_ref()
                        .is_some_and(|s| matches!(s.effect.as_ref(), Effect::Meld { .. }));
                is_meld_pay.then_some(*execute)
            })
            .expect("Vanille has a PayCost→Meld trigger")
    }

    // Both halves co-owned/controlled by P0, Ragnarok result face seeded, and
    // exactly {3}{B}{G} (one Black, one Green, three generic) in P0's pool.
    fn setup() -> (GameRunner, ObjectId, ObjectId) {
        let mut sc = GameScenario::new();
        let vanille = sc.add_creature(P0, "Vanille, Cheerful l'Cie", 3, 3).id();
        let fang = sc.add_creature(P0, "Fang, Fearless l'Cie", 4, 4).id();
        // Non-zero P/T: `GameRunner::act` runs SBAs after the meld resolves, so a
        // 0/0 result face would be destroyed (CR 704.5f) and split back before the
        // assertions. Ragnarok is 8/8; any positive P/T keeps the melded permanent
        // alive so the accept-branch merge is observable.
        let mut face = CardFace {
            name: RESULT.to_string(),
            power: Some(PtValue::Fixed(8)),
            toughness: Some(PtValue::Fixed(8)),
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), face);
        seed_meld_pair(
            &mut sc.state,
            "Vanille, Cheerful l'Cie",
            "Fang, Fearless l'Cie",
            RESULT,
        );
        sc.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Black, ObjectId(9001), false, vec![]),
                ManaUnit::new(ManaType::Green, ObjectId(9002), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(9003), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(9004), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(9005), false, vec![]),
            ],
        );
        (sc.build(), vanille, fang)
    }

    fn pool_total(runner: &GameRunner) -> usize {
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .map(|p| p.mana_pool.total())
            .unwrap_or(0)
    }

    let execute = vanille_meld_execute();

    // ── ACCEPT: pay {3}{B}{G} → the gated meld fires. ───────────────────────
    {
        let (mut runner, vanille, fang) = setup();
        let resolved = build_resolved_from_def(&execute, vanille, P0);
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
            .expect("Vanille meld execute resolves to the optional pay prompt");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "reach-guard: the you-may-pay {{3}}{{B}}{{G}} prompt must be surfaced, got {:?}",
            runner.state().waiting_for
        );

        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting the optional pay is handled");

        let survivor = runner
            .state()
            .objects
            .get(&vanille)
            .expect("Vanille persists");
        assert_eq!(
            survivor.merged_components,
            vec![vanille, fang],
            "accept: the {{3}}{{B}}{{G}} pay was performed → Vanille + Fang melded"
        );
        assert_eq!(survivor.name, RESULT, "accept: survivor presents Ragnarok");
        assert!(
            !runner.state().battlefield.iter().any(|&id| id == fang),
            "accept: Fang is absorbed into the melded permanent"
        );
        assert_eq!(
            pool_total(&runner),
            0,
            "accept: the {{3}}{{B}}{{G}} was spent from the pool"
        );
    }

    // ── DECLINE (CR 118.12 discriminator): no pay → NO meld. ────────────────
    {
        let (mut runner, vanille, fang) = setup();
        let resolved = build_resolved_from_def(&execute, vanille, P0);
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
            .expect("Vanille meld execute resolves to the optional pay prompt");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "reach-guard: the pay prompt must be surfaced before declining, got {:?}",
            runner.state().waiting_for
        );

        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("declining the optional pay is handled");

        let vanille_obj = runner
            .state()
            .objects
            .get(&vanille)
            .expect("Vanille persists");
        assert!(
            vanille_obj.merged_components.is_empty(),
            "decline: NO meld — reverting the OptionalEffectPerformed sub-gate would meld here"
        );
        assert!(
            runner.state().battlefield.iter().any(|&id| id == vanille)
                && runner.state().battlefield.iter().any(|&id| id == fang),
            "decline: both halves remain independent on the battlefield"
        );
        assert_eq!(
            pool_total(&runner),
            5,
            "decline: no mana was spent (CR 118.12: the cost is not paid)"
        );
    }
}

/// CR 508.4 / CR 701.42: Mishra's production Oracle ability selects every
/// legal defending player, planeswalker, and protected battle; the meld result
/// enters tapped and attacking the chosen destination without becoming a
/// declared attacker. A destination that disappears while the choice is open
/// produces a nonattacking entry that is still tapped.
#[test]
fn mishra_production_meld_entry_attack_destination_matrix() {
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::scenario::{GameRunner, P0, P1};
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{ChosenAttribute, EntryAttackDestination, PermanentEntryMode};
    use crate::types::actions::GameAction;

    const MISHRA: &str = "Whenever you attack, each opponent loses X life and you gain X life, \
        where X is the number of attacking creatures. If Mishra, Claimed by Gix and a creature \
        named Phyrexian Dragon Engine are attacking, and you both own and control them, exile \
        them, then meld them into Mishra, Lost to Phyrexia. It enters tapped and attacking.";
    const MISHRA_RESULT: &str = "Mishra, Lost to Phyrexia";

    fn find_meld(def: &crate::types::ability::AbilityDefinition) -> Option<Effect> {
        if matches!(def.effect.as_ref(), Effect::Meld { .. }) {
            return Some((*def.effect).clone());
        }
        def.sub_ability
            .as_deref()
            .and_then(find_meld)
            .or_else(|| def.else_ability.as_deref().and_then(find_meld))
            .or_else(|| def.mode_abilities.iter().find_map(find_meld))
    }

    fn production_effect() -> Effect {
        let parsed = parse_oracle_text(
            MISHRA,
            "Mishra, Claimed by Gix",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &[],
        );
        let effect = parsed
            .triggers
            .iter()
            .filter_map(|trigger| trigger.execute.as_deref())
            .find_map(find_meld)
            .expect("production Mishra Oracle text contains a meld child");
        assert!(matches!(
            effect,
            Effect::Meld {
                entry: PermanentEntryMode::TappedAndAttacking {
                    destination: EntryAttackDestination::AnyDefender
                },
                ..
            }
        ));
        effect
    }

    fn setup(effect: Effect) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
        let mut sc = GameScenario::new();
        let mishra = sc.add_creature(P0, "Mishra, Claimed by Gix", 3, 5).id();
        let engine = sc.add_creature(P0, "Phyrexian Dragon Engine", 2, 2).id();
        let planeswalker = sc.add_creature(P1, "Defending Planeswalker", 1, 1).id();
        let battle = sc.add_creature(P0, "Protected Battle", 1, 1).id();

        {
            let object = sc.state.objects.get_mut(&planeswalker).unwrap();
            object.card_types.core_types.clear();
            object.card_types.core_types.push(CoreType::Planeswalker);
            object.base_card_types.core_types.clear();
            object
                .base_card_types
                .core_types
                .push(CoreType::Planeswalker);
        }
        {
            let object = sc.state.objects.get_mut(&battle).unwrap();
            object.card_types.core_types.clear();
            object.card_types.core_types.push(CoreType::Battle);
            object.base_card_types.core_types.clear();
            object.base_card_types.core_types.push(CoreType::Battle);
        }
        sc.state
            .objects
            .get_mut(&battle)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Player(P1));

        let mut result = CardFace {
            name: MISHRA_RESULT.to_string(),
            power: Some(PtValue::Fixed(9)),
            toughness: Some(PtValue::Fixed(9)),
            ..CardFace::default()
        };
        result.card_type.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut sc.state.card_face_registry)
            .insert(MISHRA_RESULT.to_lowercase(), result);
        seed_meld_pair(
            &mut sc.state,
            "Mishra, Claimed by Gix",
            "Phyrexian Dragon Engine",
            MISHRA_RESULT,
        );
        sc.state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::new(mishra, AttackTarget::Player(P1), P1),
                AttackerInfo::new(engine, AttackTarget::Player(P1), P1),
            ],
            ..CombatState::default()
        });
        let mut events = Vec::new();
        perform_meld(
            &mut sc.state,
            &ResolvedAbility::new(effect, Vec::new(), mishra, P0),
            &mut events,
        )
        .expect("production Mishra meld begins");
        assert!(matches!(
            sc.state.waiting_for,
            WaitingFor::MeldAttackTargetChoice { .. }
        ));
        (sc.build(), mishra, engine, planeswalker, battle)
    }

    let effect = production_effect();
    for destination in [
        AttackTarget::Player(P1),
        AttackTarget::Planeswalker(ObjectId(0)),
        AttackTarget::Battle(ObjectId(0)),
    ] {
        let (mut runner, mishra, _engine, planeswalker, battle) = setup(effect.clone());
        let destination = match destination {
            AttackTarget::Planeswalker(_) => AttackTarget::Planeswalker(planeswalker),
            AttackTarget::Battle(_) => AttackTarget::Battle(battle),
            player => player,
        };
        let result = runner
            .act(GameAction::ChooseEntryAttackTarget {
                target: destination,
            })
            .expect("engine accepts an offered Mishra attack destination");
        let melded = &runner.state().objects[&mishra];
        assert!(melded.tapped);
        assert_eq!(melded.name, MISHRA_RESULT);
        assert!(runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == mishra && attacker.attack_target == destination));
        assert!(!result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::AttackersDeclared { .. })));
        let entry_record = result.events.iter().find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            } if *object_id == mishra => Some(record.as_ref()),
            _ => None,
        });
        let entry_record = entry_record.expect("meld entry emits its authoritative snapshot");
        assert_eq!(entry_record.name, MISHRA_RESULT);
        assert!(entry_record.combat_status.attacking);
        assert_eq!(
            entry_record.combat_status.defending_player,
            Some(P1),
            "ETB observers must see the result already attacking"
        );
    }

    let (mut runner, mishra, _engine, planeswalker, _battle) = setup(effect);
    crate::game::zones::move_to_zone(
        runner.state_mut(),
        planeswalker,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    let result = runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Planeswalker(planeswalker),
        })
        .expect("a formerly offered destination resolves through the stale fallback");
    assert!(
        runner.state().objects[&mishra].tapped,
        "the independent tapped entry modifier survives a stale attack destination"
    );
    assert!(!runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|attacker| attacker.object_id == mishra));
    assert!(!result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::AttackersDeclared { .. })));
}

/// CR 508.4 + CR 701.42: when the defending opponent is the only legal
/// destination, Mishra's production Oracle ability chooses it automatically.
/// The post-replacement liminal choice must reach both the ETB snapshot and the
/// committed combat state; no player prompt is needed for a singleton domain.
#[test]
fn mishra_single_defender_auto_target_enters_attacking() {
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{EntryAttackDestination, PermanentEntryMode};

    const MISHRA: &str = "Whenever you attack, each opponent loses X life and you gain X life, \
        where X is the number of attacking creatures. If Mishra, Claimed by Gix and a creature \
        named Phyrexian Dragon Engine are attacking, and you both own and control them, exile \
        them, then meld them into Mishra, Lost to Phyrexia. It enters tapped and attacking.";
    const MISHRA_RESULT: &str = "Mishra, Lost to Phyrexia";

    fn find_meld(def: &crate::types::ability::AbilityDefinition) -> Option<Effect> {
        if matches!(def.effect.as_ref(), Effect::Meld { .. }) {
            return Some((*def.effect).clone());
        }
        def.sub_ability
            .as_deref()
            .and_then(find_meld)
            .or_else(|| def.else_ability.as_deref().and_then(find_meld))
            .or_else(|| def.mode_abilities.iter().find_map(find_meld))
    }

    let parsed = parse_oracle_text(
        MISHRA,
        "Mishra, Claimed by Gix",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    );
    let effect = parsed
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref())
        .find_map(find_meld)
        .expect("production Mishra Oracle text contains a meld child");
    assert!(matches!(
        effect,
        Effect::Meld {
            entry: PermanentEntryMode::TappedAndAttacking {
                destination: EntryAttackDestination::AnyDefender
            },
            ..
        }
    ));

    let mut sc = GameScenario::new();
    let mishra = sc.add_creature(P0, "Mishra, Claimed by Gix", 3, 5).id();
    let engine = sc.add_creature(P0, "Phyrexian Dragon Engine", 2, 2).id();
    let mut result_face = CardFace {
        name: MISHRA_RESULT.to_string(),
        power: Some(PtValue::Fixed(9)),
        toughness: Some(PtValue::Fixed(9)),
        ..CardFace::default()
    };
    result_face.card_type.core_types.push(CoreType::Creature);
    Arc::make_mut(&mut sc.state.card_face_registry)
        .insert(MISHRA_RESULT.to_lowercase(), result_face);
    seed_meld_pair(
        &mut sc.state,
        "Mishra, Claimed by Gix",
        "Phyrexian Dragon Engine",
        MISHRA_RESULT,
    );
    sc.state.combat = Some(CombatState {
        attackers: vec![
            AttackerInfo::new(mishra, AttackTarget::Player(P1), P1),
            AttackerInfo::new(engine, AttackTarget::Player(P1), P1),
        ],
        ..CombatState::default()
    });

    let mut events = Vec::new();
    perform_meld(
        &mut sc.state,
        &ResolvedAbility::new(effect, Vec::new(), mishra, P0),
        &mut events,
    )
    .expect("production Mishra meld resolves with its singleton destination");

    assert!(matches!(sc.state.waiting_for, WaitingFor::Priority { .. }));
    assert!(sc.state.objects[&mishra].tapped);
    assert_eq!(sc.state.objects[&mishra].name, MISHRA_RESULT);
    assert!(sc
        .state
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|attacker| {
            attacker.object_id == mishra
                && attacker.attack_target == AttackTarget::Player(P1)
                && attacker.defending_player == P1
        }));
    let entry_record = events.iter().find_map(|event| match event {
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            record,
            ..
        } if *object_id == mishra => Some(record.as_ref()),
        _ => None,
    });
    let entry_record = entry_record.expect("meld entry emits its authoritative snapshot");
    assert!(entry_record.combat_status.attacking);
    assert_eq!(entry_record.combat_status.defending_player, Some(P1));
}

/// CR 614.12a + CR 707.9 + CR 508.4: an as-enters copy choice is finalized
/// before the meld entry's attacking status. Copying a noncreature therefore
/// produces a tapped, nonattacking permanent and a realized ETB snapshot.
#[test]
fn mishra_copy_as_enters_noncreature_is_not_attacking() {
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::scenario::GameRunner;
    use crate::types::ability::TargetRef;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, EntryAttackDestination, PermanentEntryMode,
        ReplacementDefinition, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CardType;
    use crate::types::replacements::ReplacementEvent;

    const SOURCE: &str = "Mishra, Claimed by Gix";
    const PARTNER: &str = "Phyrexian Dragon Engine";
    const RESULT: &str = "Mishra, Lost to Phyrexia";

    let mut sc = GameScenario::new();
    let source = sc.add_creature(P0, SOURCE, 3, 5).id();
    let partner = sc.add_creature(P0, PARTNER, 2, 2).id();
    let copy_target = sc.add_creature(P1, "Powerstone Relic", 0, 0).id();
    {
        let target = sc.state.objects.get_mut(&copy_target).unwrap();
        target.card_types = CardType::default();
        target.card_types.core_types.push(CoreType::Artifact);
        target.base_card_types = target.card_types.clone();
        target.name = "Powerstone Relic".to_string();
        target.base_name = target.name.clone();
    }

    let mut result_face = CardFace {
        name: RESULT.to_string(),
        power: Some(PtValue::Fixed(9)),
        toughness: Some(PtValue::Fixed(9)),
        ..CardFace::default()
    };
    result_face.card_type.core_types.push(CoreType::Creature);
    result_face.replacements.push(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::SpecificObject { id: copy_target },
                    recipient: TargetFilter::SelfRef,
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            )),
    );
    Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), result_face);
    seed_meld_pair(&mut sc.state, SOURCE, PARTNER, RESULT);
    sc.state.combat = Some(CombatState {
        attackers: vec![
            AttackerInfo::new(source, AttackTarget::Player(P1), P1),
            AttackerInfo::new(partner, AttackTarget::Player(P1), P1),
        ],
        ..CombatState::default()
    });
    let effect = Effect::Meld {
        source: SOURCE.to_string(),
        partner: PARTNER.to_string(),
        result: RESULT.to_string(),
        source_filter: TargetFilter::SelfRef,
        partner_filter: TargetFilter::Any,
        entry: PermanentEntryMode::TappedAndAttacking {
            destination: EntryAttackDestination::AnyDefender,
        },
    };

    let mut first_events = Vec::new();
    perform_meld(
        &mut sc.state,
        &ResolvedAbility::new(effect, Vec::new(), source, P0),
        &mut first_events,
    )
    .unwrap();
    assert!(matches!(
        sc.state.waiting_for,
        WaitingFor::CopyTargetChoice { .. }
    ));
    assert!(
        !first_events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                ..
            } if *object_id == source
        )),
        "the frontend entry event waits for the final copy snapshot"
    );

    let mut runner = GameRunner::from_state(sc.state);
    let result = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(copy_target)),
        })
        .expect("copy target choice resolves");
    let copied = &runner.state().objects[&source];
    assert_eq!(copied.name, "Powerstone Relic");
    assert!(copied.tapped);
    assert!(!copied.card_types.core_types.contains(&CoreType::Creature));
    assert!(!runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|attacker| attacker.object_id == source));
    let record = result.events.iter().find_map(|event| match event {
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            record,
            ..
        } if *object_id == source => Some(record.as_ref()),
        _ => None,
    });
    let record = record.expect("the realized entry snapshot is emitted after the copy choice");
    assert_eq!(record.name, "Powerstone Relic");
    assert!(!record.combat_status.attacking);
}

/// CR 613.1b + CR 508.4: a layer-2 control effect that starts applying only
/// after the meld identity exists makes the final controller choose among that
/// attacking team's legal destinations.
#[test]
fn mishra_final_controller_owns_attack_destination_choice() {
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::scenario::GameRunner;
    use crate::types::ability::{
        ContinuousModification, Duration, EntryAttackDestination, FilterProp, PermanentEntryMode,
        TargetFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::format::FormatConfig;

    const SOURCE: &str = "Mishra, Claimed by Gix";
    const PARTNER: &str = "Phyrexian Dragon Engine";
    const RESULT: &str = "Mishra, Lost to Phyrexia";

    let mut sc = GameScenario::new();
    sc.state = crate::types::game_state::GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    sc.state.active_player = PlayerId(0);
    let source = sc.add_creature(PlayerId(0), SOURCE, 3, 5).id();
    let partner = sc.add_creature(PlayerId(0), PARTNER, 2, 2).id();
    let control_source = sc.add_creature(PlayerId(1), "Result Controller", 1, 1).id();
    let mut result_face = CardFace {
        name: RESULT.to_string(),
        power: Some(PtValue::Fixed(9)),
        toughness: Some(PtValue::Fixed(9)),
        ..CardFace::default()
    };
    result_face.card_type.core_types.push(CoreType::Creature);
    Arc::make_mut(&mut sc.state.card_face_registry).insert(RESULT.to_lowercase(), result_face);
    seed_meld_pair(&mut sc.state, SOURCE, PARTNER, RESULT);
    sc.state.add_transient_continuous_effect(
        control_source,
        PlayerId(1),
        Duration::Permanent,
        TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Named {
            name: RESULT.to_string(),
        }])),
        vec![ContinuousModification::ChangeController],
        None,
    );
    sc.state.combat = Some(CombatState {
        attackers: vec![
            AttackerInfo::new(source, AttackTarget::Player(PlayerId(2)), PlayerId(2)),
            AttackerInfo::new(partner, AttackTarget::Player(PlayerId(2)), PlayerId(2)),
        ],
        ..CombatState::default()
    });
    let effect = Effect::Meld {
        source: SOURCE.to_string(),
        partner: PARTNER.to_string(),
        result: RESULT.to_string(),
        source_filter: TargetFilter::SelfRef,
        partner_filter: TargetFilter::Any,
        entry: PermanentEntryMode::TappedAndAttacking {
            destination: EntryAttackDestination::AnyDefender,
        },
    };

    let mut events = Vec::new();
    perform_meld(
        &mut sc.state,
        &ResolvedAbility::new(effect, Vec::new(), source, PlayerId(0)),
        &mut events,
    )
    .unwrap();
    let WaitingFor::MeldAttackTargetChoice {
        player,
        ref valid_targets,
        ..
    } = sc.state.waiting_for
    else {
        panic!("final controller must receive the attack-destination choice")
    };
    assert_eq!(player, PlayerId(1));
    assert!(valid_targets.contains(&AttackTarget::Player(PlayerId(2))));
    assert!(valid_targets.contains(&AttackTarget::Player(PlayerId(3))));
    assert_eq!(sc.state.objects[&source].controller, PlayerId(1));

    let mut runner = GameRunner::from_state(sc.state);
    let result = runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(PlayerId(3)),
        })
        .expect("final controller chooses a defending-team destination");
    assert!(runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|attacker| {
            attacker.object_id == source
                && attacker.attack_target == AttackTarget::Player(PlayerId(3))
        }));
    let record = result.events.iter().find_map(|event| match event {
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            record,
            ..
        } if *object_id == source => Some(record.as_ref()),
        _ => None,
    });
    let record = record.expect("choice emits the finalized entry snapshot");
    assert_eq!(record.controller, PlayerId(1));
    assert!(record.combat_status.attacking);
    assert_eq!(record.combat_status.defending_player, Some(PlayerId(3)));
}
