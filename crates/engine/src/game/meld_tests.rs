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
}

/// A meld `ResolvedAbility` whose source is `source`, controlled by `controller`,
/// melding with `partner` into Brisela.
fn meld_ability(source: ObjectId, controller: PlayerId, partner: &str) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Meld {
            source: "Gisela, the Broken Blade".to_string(),
            partner: partner.to_string(),
            result: RESULT_NAME.to_string(),
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
/// card and cannot be melded — the resolver no-ops. DISCRIMINATING: the pre-fix
/// finder gated only on name, so the token partner was melded.
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
    assert_eq!(
        src.zone,
        Zone::Battlefield,
        "no-op: a token partner is not a real meld card, so the source stays put"
    );
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred with a token partner"
    );
    assert!(
        state.exile.is_empty(),
        "nothing was exiled — the token partner is not a meld half"
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
    assert_eq!(
        src.zone,
        Zone::Battlefield,
        "no-op: a copy partner is not a real meld card"
    );
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred with a copy partner"
    );
    assert!(
        state.exile.is_empty(),
        "nothing was exiled — the copy partner is not a meld half"
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

    // No-op: the impostor's printed identity is not the meld half.
    let src = state.objects.get(&source).expect("source persists");
    assert_eq!(
        src.zone,
        Zone::Battlefield,
        "no-op: the renamed non-meld impostor is rejected by the base_name gate"
    );
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred against a renamed impostor"
    );
    let imp = state.objects.get(&impostor).expect("impostor persists");
    assert_eq!(
        imp.zone,
        Zone::Battlefield,
        "the impostor is not exiled or absorbed"
    );
    assert!(
        state.exile.is_empty(),
        "nothing was exiled — the impostor is not a real meld half"
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
    assert_eq!(
        src.zone,
        Zone::Battlefield,
        "no-op: the source's printed identity is not the meld instigator"
    );
    assert!(
        src.merged_components.is_empty(),
        "no meld occurred with a non-meld source"
    );
    assert_eq!(
        state.objects.get(&partner).expect("partner persists").zone,
        Zone::Battlefield,
        "the real partner is not exiled or absorbed"
    );
    assert!(
        state.exile.is_empty(),
        "nothing was exiled — the source is not the real meld instigator"
    );
}

/// CR 614.1c / CR 614.12a: the survivor's exile→battlefield entry is routed
/// through the zone-change pipeline, so an entry replacement effect on the
/// survivor (here: enters-tapped) is consulted. DISCRIMINATING: the pre-fix raw
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

    // A self-scoped "enters tapped" replacement on the survivor (CR 614.1c /
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
    {
        let obj = state.objects.get_mut(&source).unwrap();
        obj.replacement_definitions.push(enters_tapped.clone());
        // The survivor is reverted to its base characteristics on the exile leg
        // of the meld (CR 613.1 zone-exit reset restores `replacement_definitions`
        // from `base_replacement_definitions`). Seed the base too so this printed
        // replacement survives the exile→battlefield round-trip — a real meld
        // card's printed "enters tapped" replacement lives in its base.
        obj.base_replacement_definitions = std::sync::Arc::new(vec![enters_tapped]);
    }

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
    let mut state = sc.state;

    let ability = ResolvedAbility::new(
        Effect::Meld {
            source: src_name,
            partner: partner_name,
            result: result_name,
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
