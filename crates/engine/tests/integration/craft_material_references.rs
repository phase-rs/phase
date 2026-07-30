//! CR 702.167c — "an ability of a permanent may refer to the exiled cards used
//! to craft it." This batch teaches the quantity / static / mana parsers to read
//! the persistent `ExileLinkKind::CraftMaterial` linked-exile pool (the cards
//! exiled to pay the craft activation cost) via the existing kind-agnostic
//! `TargetFilter::ExiledBySource` consumer. No new engine variant — the four
//! references reuse `QuantityRef::Aggregate`, `DistinctColorsAmongPermanents`,
//! and `ManaProduction::DistinctColorsAmongPermanents` parameterized with the
//! `ExiledBySource` filter.
//!
//! Two layers per claim:
//!   1. Parse coverage — each shipped craft-reference line parses with zero
//!      `Effect::Unimplemented` and emits the typed `ExiledBySource` ref.
//!   2. Runtime discrimination — a craft source on the battlefield with materials
//!      linked via `CraftMaterial` resolves the reference through the SAME
//!      production paths the engine uses (CDA layer evaluator, GainLife resolver,
//!      mana-ability resolver). Each assertion flips if the parser change is
//!      reverted (the phrase would parse to `Unimplemented` / `TrackedSetAggregate`
//!      which resolves to 0 with no preceding chain set).

use engine::game::layers::evaluate_layers;
use engine::game::quantity::resolve_quantity;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AggregateFunction, ContinuousModification, Effect, ManaProduction, ObjectProperty,
    QuantityExpr, QuantityRef, StaticDefinition, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

fn artifact_creature_types() -> Vec<String> {
    vec!["Artifact".to_string(), "Creature".to_string()]
}

fn color_shard(c: ManaColor) -> ManaCostShard {
    match c {
        ManaColor::White => ManaCostShard::White,
        ManaColor::Blue => ManaCostShard::Blue,
        ManaColor::Black => ManaCostShard::Black,
        ManaColor::Red => ManaCostShard::Red,
        ManaColor::Green => ManaCostShard::Green,
    }
}

/// Build a craft source on the battlefield (the returned, transformed permanent)
/// plus N material cards in exile, each linked to the source via
/// `ExileLinkKind::CraftMaterial`. Returns (state, source_id, material_ids).
fn craft_state_with_materials(
    materials: &[(i32, i32, &[ManaColor], u32)], // (power, toughness, colors, mana_value)
) -> (GameState, ObjectId, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(1000),
        P0,
        "Craft Source".to_string(),
        Zone::Battlefield,
    );
    {
        let src = state.objects.get_mut(&source_id).unwrap();
        src.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        src.base_card_types = src.card_types.clone();
        src.power = Some(0);
        src.toughness = Some(0);
        src.base_power = Some(0);
        src.base_toughness = Some(0);
    }

    let mut ids = Vec::new();
    for (i, (power, toughness, colors, mv)) in materials.iter().enumerate() {
        let id = create_object(
            &mut state,
            CardId(2000 + i as u64),
            P0,
            format!("Material {i}"),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(*power);
        obj.toughness = Some(*toughness);
        obj.base_power = Some(*power);
        obj.base_toughness = Some(*toughness);
        obj.color = colors.to_vec();
        obj.mana_cost = ManaCost::Cost {
            shards: colors.iter().map(|c| color_shard(*c)).collect(),
            generic: mv.saturating_sub(colors.len() as u32),
        };
        state.exile_links.push(ExileLink {
            exiled_id: id,
            source_id,
            kind: ExileLinkKind::CraftMaterial,
        });
        ids.push(id);
    }
    (state, source_id, ids)
}

/// Parse a single oracle line and return its sole static definition.
fn parse_cda_static(oracle: &str) -> StaticDefinition {
    let parsed = parse_oracle_text(oracle, "~", &[], &artifact_creature_types(), &[]);
    assert_eq!(
        parsed.statics.len(),
        1,
        "expected exactly one static, got {:#?}",
        parsed.statics
    );
    parsed.statics[0].clone()
}

// ---------------------------------------------------------------------------
// Mastercraft Raptor — power = total power of the exiled cards used to craft it.
// ---------------------------------------------------------------------------

#[test]
fn mastercraft_raptor_power_is_total_power_of_craft_materials() {
    // Parse layer: the CDA must SetDynamicPower to Aggregate{Sum, Power, ExiledBySource…}.
    let static_def = parse_cda_static(
        "Mastercraft Raptor's power is equal to the total power of the exiled cards used to craft it.",
    );
    let power_qty = static_def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::SetDynamicPower { value } => Some(value),
            _ => None,
        })
        .expect("CDA must set dynamic power");
    // Revert guard: pre-fix this phrase parsed to Unimplemented (no static), so
    // `parsed.statics.len() == 1` would already fail. The aggregate shape below
    // additionally pins the source to the linked-exile pool, not a tracked set.
    match power_qty {
        QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                    filter,
                },
        } => assert!(
            filter_reads_exiled_by_source(filter),
            "power aggregate must read ExiledBySource, got {filter:?}"
        ),
        other => panic!("expected Aggregate{{Sum, Power, ExiledBySource}}, got {other:?}"),
    }

    // Runtime layer: 4/4 + 1/1 + 2/3 materials → total power 7. Resolve through
    // the production quantity resolver (the CDA layer evaluator calls this).
    let (state, source_id, _) = craft_state_with_materials(&[
        (4, 4, &[ManaColor::Red], 4),
        (1, 1, &[ManaColor::Green], 1),
        (2, 3, &[ManaColor::White], 2),
    ]);
    let resolved = resolve_quantity(&state, power_qty, P0, source_id);
    assert_eq!(
        resolved, 7,
        "Mastercraft Raptor power must equal total power (4+1+2) of craft materials in exile"
    );

    // Discriminating negative: with NO linked materials the aggregate is 0.
    let (empty_state, empty_src, _) = craft_state_with_materials(&[]);
    assert_eq!(
        resolve_quantity(&empty_state, power_qty, P0, empty_src),
        0,
        "with no craft materials the total-power aggregate must be 0"
    );
}

// ---------------------------------------------------------------------------
// Sunbird Effigy — P/T = number of colors among the exiled cards used to craft it.
// ---------------------------------------------------------------------------

#[test]
fn sunbird_effigy_pt_is_distinct_colors_of_craft_materials() {
    let static_def = parse_cda_static(
        "Sunbird Effigy's power and toughness are each equal to the number of colors among the exiled cards used to craft it.",
    );
    let mut power = None;
    let mut toughness = None;
    for m in &static_def.modifications {
        match m {
            ContinuousModification::SetDynamicPower { value } => power = Some(value.clone()),
            ContinuousModification::SetDynamicToughness { value } => {
                toughness = Some(value.clone())
            }
            _ => {}
        }
    }
    let power = power.expect("must set dynamic power");
    let toughness = toughness.expect("must set dynamic toughness");
    for (label, qty) in [("power", &power), ("toughness", &toughness)] {
        match qty {
            QuantityExpr::Ref {
                qty: QuantityRef::DistinctColorsAmongPermanents { filter },
            } => assert!(
                filter_reads_exiled_by_source(filter),
                "{label} colors must read ExiledBySource, got {filter:?}"
            ),
            other => panic!("{label}: expected DistinctColorsAmongPermanents, got {other:?}"),
        }
    }

    // Runtime: materials are W, U, and W again → 2 distinct colors.
    let (state, source_id, _) = craft_state_with_materials(&[
        (1, 1, &[ManaColor::White], 1),
        (1, 1, &[ManaColor::Blue], 1),
        (1, 1, &[ManaColor::White], 1),
    ]);
    assert_eq!(
        resolve_quantity(&state, &power, P0, source_id),
        2,
        "Sunbird power must equal distinct colors (W, U) among craft materials"
    );
    assert_eq!(
        resolve_quantity(&state, &toughness, P0, source_id),
        2,
        "Sunbird toughness must equal distinct colors among craft materials"
    );

    // Discriminating: a colorless material set yields 0.
    let (colorless, cl_src, _) = craft_state_with_materials(&[(1, 1, &[], 2)]);
    assert_eq!(
        resolve_quantity(&colorless, &power, P0, cl_src),
        0,
        "colorless craft materials contribute no colors"
    );
}

// ---------------------------------------------------------------------------
// Sunbird Effigy mana — {T}: For each color among the exiled cards used to craft
// this creature, add one mana of that color.
// ---------------------------------------------------------------------------

#[test]
fn sunbird_effigy_mana_is_one_per_color_of_craft_materials() {
    let parsed = parse_oracle_text(
        "{T}: For each color among the exiled cards used to craft this creature, add one mana of that color.",
        "~",
        &[],
        &artifact_creature_types(),
        &[],
    );
    assert_eq!(
        parsed.abilities.len(),
        1,
        "expected one activated mana ability"
    );
    let Effect::Mana { produced, .. } = parsed.abilities[0].effect.as_ref() else {
        panic!(
            "expected a Mana effect, got {:?}",
            parsed.abilities[0].effect
        );
    };
    // Revert guard: pre-fix this body parsed to Unimplemented, not Mana.
    match produced {
        ManaProduction::DistinctColorsAmongPermanents { filter } => assert!(
            filter_reads_exiled_by_source(filter),
            "mana production must read ExiledBySource, got {filter:?}"
        ),
        other => panic!("expected DistinctColorsAmongPermanents mana, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Jadeheart Attendant — gain life equal to the mana value of the exiled card
// used to craft it.
// ---------------------------------------------------------------------------

#[test]
fn jadeheart_attendant_gains_life_equal_to_craft_material_mana_value() {
    let parsed = parse_oracle_text(
        "When this creature enters, you gain life equal to the mana value of the exiled card used to craft it.",
        "~",
        &[],
        &artifact_creature_types(),
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "expected one ETB trigger");
    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("ETB trigger must carry an execute ability");
    let Effect::GainLife { amount, .. } = execute.effect.as_ref() else {
        panic!("expected GainLife effect, got {:?}", execute.effect);
    };
    // Revert guard: pre-fix this parsed to Unimplemented (name="gain").
    match amount {
        QuantityExpr::Ref {
            qty:
                QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::ManaValue,
                    filter,
                },
        } => assert!(
            filter_reads_exiled_by_source(filter),
            "life amount must read ExiledBySource mana value, got {filter:?}"
        ),
        other => panic!("expected Aggregate{{Sum, ManaValue, ExiledBySource}}, got {other:?}"),
    }

    // Runtime: a single mana-value-5 craft material → gain 5.
    let (state, source_id, _) =
        craft_state_with_materials(&[(2, 2, &[ManaColor::White, ManaColor::White], 5)]);
    assert_eq!(
        resolve_quantity(&state, amount, P0, source_id),
        5,
        "Jadeheart life gain must equal the mana value (5) of the exiled craft material"
    );
}

// ---------------------------------------------------------------------------
// CDA layer end-to-end: the parsed static, applied through evaluate_layers, sets
// the source's live power. This drives the production layer pipeline, not just a
// direct quantity resolve.
// ---------------------------------------------------------------------------

#[test]
fn craft_cda_power_applies_through_layer_evaluator() {
    let parsed = parse_oracle_text(
        "Mastercraft Raptor's power is equal to the total power of the exiled cards used to craft it.",
        "~",
        &[],
        &artifact_creature_types(),
        &[],
    );
    let static_def = parsed.statics[0].clone();

    let (mut state, source_id, _) = craft_state_with_materials(&[
        (3, 3, &[ManaColor::Black], 3),
        (2, 2, &[ManaColor::Green], 2),
    ]);
    // Attach the parsed CDA static to the source. The layer evaluator rebuilds
    // `static_definitions` from `base_static_definitions` each pass, so seed both.
    {
        let src = state.objects.get_mut(&source_id).unwrap();
        src.static_definitions.push(static_def.clone());
        let base = std::sync::Arc::make_mut(&mut src.base_static_definitions);
        base.push(static_def);
    }
    evaluate_layers(&mut state);
    let live_power = state.objects.get(&source_id).unwrap().power;
    assert_eq!(
        live_power,
        Some(5),
        "after layer evaluation the craft source power must be total material power (3+2)=5"
    );
}

/// True if `filter` resolves the source's linked-exile pool (directly
/// `ExiledBySource` or an `And`/`Or` that contains it).
fn filter_reads_exiled_by_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::ExiledBySource => true,
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_reads_exiled_by_source)
        }
        _ => false,
    }
}
