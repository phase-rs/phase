//! Coordinated card populations through the production casting pipeline.
use super::support::shared_card_export_json;
use engine::database::card_db::CardDatabase;
use engine::game::game_object::BackFaceData;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, ContinuousModification, ControllerRef, Duration, Effect, FilterProp, QuantityExpr,
    QuantityRef, StaticCondition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;
use serde_json::{to_string, Map, Value};
use std::path::Path;

const SAILORS: &str = "This spell costs {1} less to cast for each card you own in exile and in your graveyard that's an instant card, a sorcery card, or a card that has an Adventure.\nWard {4} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {4}.)";

const LOCKET: &str = "Spells you cast cost {1} less to cast for each card with the same name as that spell in your graveyard.";
const BEHOLD: &str = "Scry 2, then draw two cards.\nForetell {1}{U} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";

fn synthetic_name_face(
    name: &str,
    layout: LayoutKind,
    cost: ManaCost,
    oracle: &str,
) -> BackFaceData {
    let parsed = engine::parser::parse_oracle_text(oracle, name, &[], &["Instant".into()], &[]);
    assert!(parsed.parse_warnings.is_empty());
    assert!(parsed
        .abilities
        .iter()
        .all(|ability| !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. })));
    BackFaceData {
        name: name.into(),
        layout_kind: Some(layout),
        mana_cost: cost,
        card_types: engine::types::card_type::CardType {
            core_types: vec![engine::types::card_type::CoreType::Instant],
            ..Default::default()
        },
        abilities: parsed.abilities,
        ..Default::default()
    }
}

#[test]
fn locket_selected_face_names() {
    use engine::types::game_state::{CastingVariant, CastingVariantFace};
    // Synthetic split witnesses exercise selected half, two off-stack names,
    // and one contribution from a card sharing both fused names.
    for (variant, face, paid, gain, name) in [
        (
            CastingVariant::Normal,
            CastingVariantFace::Left,
            5,
            1,
            "Synthetic Alpha",
        ),
        (
            CastingVariant::Normal,
            CastingVariantFace::Right,
            1,
            2,
            "Synthetic Beta",
        ),
        (
            CastingVariant::Fuse,
            CastingVariantFace::Left,
            7,
            3,
            "Synthetic Alpha",
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, paid);
        add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic Alpha", true, "You gain 1 life.")
            .with_mana_cost(blue_cost(5, 1))
            .with_keyword(Keyword::Fuse)
            .id();
        let both = scenario
            .add_spell_to_graveyard(P0, "Synthetic Beta", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic Beta", true);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .unwrap()
            .back_face = Some(synthetic_name_face(
            "Synthetic Beta",
            LayoutKind::Split,
            blue_cost(2, 1),
            "You gain 2 life.",
        ));
        runner.state_mut().objects.get_mut(&both).unwrap().back_face = Some(synthetic_name_face(
            "Synthetic Alpha",
            LayoutKind::Split,
            blue_cost(5, 1),
            "You gain 1 life.",
        ));
        assert_eq!(runner.state().objects[&both].effective_names().len(), 2);
        let committed = runner
            .cast(spell)
            .casting_variant_face(variant, face)
            .commit();
        assert_eq!(committed.state().objects[&spell].name, name);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        assert_eq!(
            committed.selected_casting_variant().unwrap().variant,
            variant
        );
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, gain);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
}

#[test]
fn locket_adventure_and_modal_names() {
    use engine::types::game_state::CastingVariant;
    for creature_face in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if creature_face { 3 } else { 2 });
        add_locket(&mut scenario);
        let spell = scenario
            .add_creature_to_hand(P0, "Synthetic adventurer", 2, 2)
            .with_mana_cost(blue_cost(3, 1))
            .id();
        let ordinary = scenario
            .add_creature_to_graveyard(P0, "Synthetic adventurer", 2, 2)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic Journey", true);
        let mut runner = scenario.build();
        for id in [spell, ordinary] {
            runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(synthetic_name_face(
                "Synthetic Journey",
                LayoutKind::Adventure,
                blue_cost(2, 1),
                "You gain 2 life.",
            ));
        }
        let committed = runner.cast(spell).adventure_face(creature_face).commit();
        assert_eq!(
            committed.state().objects[&spell].name,
            if creature_face {
                "Synthetic adventurer"
            } else {
                "Synthetic Journey"
            }
        );
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(
            &[spell],
            if creature_face {
                Zone::Battlefield
            } else {
                Zone::Exile
            },
        );
        outcome.assert_life_delta(P0, if creature_face { 0 } else { 2 });
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    for back in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if back { 2 } else { 4 });
        add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic modal front", true, "You gain 1 life.")
            .with_mana_cost(blue_cost(3, 1))
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic modal back", true);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .unwrap()
            .back_face = Some(synthetic_name_face(
            "Synthetic modal back",
            LayoutKind::Modal,
            blue_cost(2, 1),
            "You gain 2 life.",
        ));
        let committed = runner.cast(spell).modal_back_face(back).commit();
        assert_eq!(
            committed.state().objects[&spell].name,
            if back {
                "Synthetic modal back"
            } else {
                "Synthetic modal front"
            }
        );
        assert_eq!(
            committed.state().objects[&spell].spell_names_for(CastingVariant::Normal),
            vec![if back {
                "synthetic modal back"
            } else {
                "synthetic modal front"
            }]
        );
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, if back { 2 } else { 1 });
    }
}

#[test]
fn locket_unnamed_and_actual_face_down_nonmatch() {
    use engine::types::game_state::CastingVariant;
    for name in ["Synthetic named creature", ""] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if name.is_empty() { 4 } else { 3 });
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_creature_to_hand(P0, name, 2, 2)
            .with_mana_cost(blue_cost(3, 1))
            .id();
        scenario.add_creature_to_graveyard(P0, name, 2, 2);
        let mut runner = scenario.build();
        assert_eq!(
            locket_count(&runner, source, spell),
            if name.is_empty() { 0 } else { 1 }
        );
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    for face_down in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 3);
        add_locket(&mut scenario);
        let spell = scenario
            .add_creature_to_hand(P0, "Synthetic morph creature", 4, 4)
            .with_keyword(Keyword::Morph(ManaCost::generic(2)))
            .with_mana_cost(ManaCost::generic(4))
            .id();
        scenario.add_creature_to_graveyard(P0, "Synthetic morph creature", 4, 4);
        scenario.add_creature_to_graveyard(P0, "", 2, 2);
        let mut runner = scenario.build();
        let variant = if face_down {
            CastingVariant::FaceDown
        } else {
            CastingVariant::Normal
        };
        let committed = runner.cast(spell).casting_variant(variant).commit();
        assert_eq!(
            committed.state().objects[&spell]
                .spell_names_for(variant)
                .is_empty(),
            face_down
        );
        assert_eq!(
            committed.state().players[0].mana_pool.total(),
            0,
            "named {4}-1 and unnamed face-down {3} are separate payable branches"
        );
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.state().objects[&spell].face_down, face_down);
        assert_eq!(
            outcome.state().objects[&spell].power,
            Some(if face_down { 2 } else { 4 })
        );
    }
}

#[test]
fn locket_reference_serde_and_loaded_cast() {
    use engine::types::ability::StaticDefinition;
    // Literal legacy tags are compatibility fixtures, not derived expected encodings.
    for (json, filter) in [
        (r#"{"type":"SelfRef"}"#, TargetFilter::SelfRef),
        (r#"{"type":"ParentTarget"}"#, TargetFilter::ParentTarget),
        (r#"{"type":"StackSpell"}"#, TargetFilter::StackSpell),
        (r#"{"type":"CastingSpell"}"#, TargetFilter::CastingSpell),
    ] {
        let loaded: TargetFilter = serde_json::from_str(json).unwrap();
        assert_eq!(loaded, filter);
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::from_str::<Value>(json).unwrap()
        );
    }
    let definition = locket_definition();
    let encoded = serde_json::to_string(&definition).unwrap();
    let loaded: StaticDefinition = serde_json::from_str(&encoded).unwrap();
    assert_eq!(loaded, definition);
    let (runner, source, spell) = locket_board(2, 4);
    let state_json = serde_json::to_string(runner.state()).unwrap();
    let state: GameState = serde_json::from_str(&state_json).unwrap();
    assert_eq!(state.objects[&source].static_definitions[0], loaded);
    let mut runner = GameRunner::from_state(state);
    assert_eq!(locket_count(&runner, source, spell), 2);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_life_delta(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn locket_export_string_loader_compatibility() {
    let raw =
        shared_card_export_json().expect("fresh full export required at the run-end boundary");
    let entry = raw.get("locket of yesterdays").expect("exported Locket");
    assert_eq!(
        entry.get("oracle_text").and_then(Value::as_str),
        Some(LOCKET)
    );
    let mut entries = Map::new();
    entries.insert("locket of yesterdays".to_string(), entry.clone());
    let db = CardDatabase::from_json_str(&to_string(&entries).unwrap()).unwrap();
    let face = db.get_face_by_name("Locket of Yesterdays").unwrap();
    assert_eq!(face.static_abilities.as_slice(), &[locket_definition()]);
    let (mut runner, source, spell) = locket_board(2, 4);
    let object = runner.state_mut().objects.get_mut(&source).unwrap();
    object.static_definitions = face.static_abilities.clone().into();
    object.base_static_definitions = std::sync::Arc::new(face.static_abilities.clone());
    let outcome = runner.cast(spell).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

fn locket_definition() -> engine::types::ability::StaticDefinition {
    use engine::types::ability::{SharedQuality, SharedQualityRelation};
    let parsed = engine::parser::parse_oracle_text(
        LOCKET,
        "Locket of Yesterdays",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert!(parsed.parse_warnings.is_empty());
    assert!(parsed.abilities.is_empty());
    assert_eq!(parsed.statics.len(), 1);
    let definition = parsed.statics[0].clone();
    assert_eq!(
        definition.affected,
        Some(TargetFilter::Typed(
            TypedFilter::card().controller(ControllerRef::You)
        ))
    );
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(quantity),
        spell_filter,
        ..
    } = &definition.mode
    else {
        panic!("complete Locket modifier")
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert!(spell_filter.is_none());
    assert_eq!(
        *quantity,
        QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::card()),
                    TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::SharesQuality {
                            quality: SharedQuality::Name,
                            reference: Some(Box::new(TargetFilter::CastingSpell)),
                            relation: SharedQualityRelation::Shares,
                        }
                    ])),
                    TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard
                        },
                        FilterProp::Owned {
                            controller: ControllerRef::You
                        }
                    ])),
                ]
            }
        }
    );
    definition
}

fn add_locket(scenario: &mut GameScenario) -> ObjectId {
    let expected = locket_definition();
    let id = scenario
        .add_artifact_from_oracle(P0, "Locket of Yesterdays", LOCKET)
        .id();
    // The fixture itself is parsed from the complete printed Oracle, not injected AST.
    assert!(matches!(expected.mode, StaticMode::ModifyCost { .. }));
    id
}

fn locket_count(runner: &GameRunner, source: ObjectId, spell: ObjectId) -> i32 {
    let StaticMode::ModifyCost {
        dynamic_count: Some(qty),
        ..
    } = locket_definition().mode
    else {
        unreachable!()
    };
    engine::game::quantity::resolve_quantity_with_spell(
        runner.state(),
        &QuantityExpr::Ref { qty },
        P0,
        source,
        spell,
    )
}

fn locket_board(matches: usize, pool: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, pool);
    let source = add_locket(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic named instant", true, "You gain 1 life.")
        .with_mana_cost(blue_cost(5, 1))
        .id();
    for _ in 0..matches {
        scenario.add_spell_to_graveyard(P0, "Synthetic named instant", true);
    }
    for name in [
        "Locket of Yesterdays",
        "Different name one",
        "Different name two",
    ] {
        scenario.add_spell_to_graveyard(P0, name, true);
    }
    scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic named instant", true, "You gain 1 life.")
        .with_mana_cost(blue_cost(5, 1));
    scenario.add_spell_to_exile(P0, "Synthetic named instant", true);
    scenario.add_creature(P0, "Synthetic named instant", 1, 1);
    scenario.add_spell_to_graveyard(P1, "Synthetic named instant", true);
    (scenario.build(), source, spell)
}

#[test]
fn locket_name_population_cost() {
    // CR 118.7a: only generic mana is reduced, with a zero generic floor.
    for (matches, pool) in [(2, 4), (0, 6), (8, 1)] {
        let (mut runner, source, spell) = locket_board(matches, pool);
        assert_eq!(locket_count(&runner, source, spell), matches as i32);
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(blue_cost(5u32.saturating_sub(matches as u32), 1))
        );
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, 1);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    let (mut runner, source, spell) = locket_board(2, 3);
    assert_eq!(locket_count(&runner, source, spell), 2);
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "one short after the payable sibling"
    );
    let (mut runner, source, spell) = locket_board(2, 4);
    assert_eq!(locket_count(&runner, source, spell), 2);
    runner.state_mut().players[0].mana_pool = (0..4)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "generic reduction cannot pay blue"
    );
}

#[test]
fn locket_independent_cast_authorities() {
    use engine::types::ability::{CardPlayMode, StaticDefinition};
    use engine::types::statics::{CastFrequency, GraveyardPermissionPool};
    for (sources, opponent_control, foreign_spell) in
        [(2, false, false), (1, true, false), (1, false, true)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let payable = if opponent_control { 6 } else { 6 - sources * 2 };
        mana(&mut scenario, payable);
        let source = add_locket(&mut scenario);
        if sources == 2 {
            add_locket(&mut scenario);
        }
        // Foreign ownership of a functioning source does not change "your".
        let spell = if foreign_spell {
            scenario
                .add_spell_to_graveyard(P1, "Authority instant", true)
                .from_oracle_text("You gain 1 life.")
                .with_mana_cost(blue_cost(5, 1))
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Authority instant", true, "You gain 1 life.")
                .with_mana_cost(blue_cost(5, 1))
                .id()
        };
        for _ in 0..2 {
            scenario.add_spell_to_graveyard(P0, "Authority instant", true);
        }
        scenario.add_spell_to_graveyard(P0, "Locket of Yesterdays", true);
        scenario.add_spell_to_graveyard(P1, "Authority instant", true);
        if foreign_spell {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic any-graveyard name permission", "")
                .with_static_definition(
                    StaticDefinition::new(StaticMode::GraveyardCastPermission {
                        frequency: CastFrequency::Unlimited,
                        play_mode: CardPlayMode::Cast,
                        graveyard_destination_replacement: None,
                        extra_cost: None,
                        enters_with_counter: None,
                        required_cast_keyword: None,
                        pool: GraveyardPermissionPool::AnyGraveyard,
                    })
                    .affected(TargetFilter::Typed(TypedFilter::card())),
                );
        }
        let mut runner = scenario.build();
        let object = runner.state_mut().objects.get_mut(&source).unwrap();
        object.owner = P1;
        if opponent_control {
            object.controller = P1;
            object.base_controller = Some(P1);
        }
        assert_eq!(locket_count(&runner, source, spell), 2);
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, 1);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert_eq!(outcome.state().objects[&source].owner, P1);
    }
    let (mut runner, source, spell) = locket_board(2, 4);
    runner.cast(spell).resolve().assert_life_delta(P0, 1);
    let next = runner.state().players[0]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Synthetic named instant")
        .unwrap();
    let source_object = runner.state_mut().objects.get_mut(&source).unwrap();
    source_object.controller = P1;
    source_object.base_controller = Some(P1);
    runner.state_mut().players[0].mana_pool = (0..6)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    let outcome = runner.cast(next).resolve();
    outcome.assert_life_delta(P0, 1);
    outcome.assert_zone(&[next], Zone::Graveyard);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the subsequent cast pays full price after source control changes"
    );
}

#[test]
fn locket_no_stack_or_target_fallback() {
    use engine::game::filter::{matches_target_filter, CastSpellContext, FilterContext};
    use engine::types::ability::{SharedQuality, SharedQualityRelation};
    use engine::types::game_state::CastingVariant;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 9);
    let source = add_locket(&mut scenario);
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(P0, "Unrelated stack name", true, "You gain 2 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let target = scenario
        .add_creature(P1, "Unrelated target name", 2, 2)
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "First priced instant",
            true,
            "Target creature gets +1/+1 until end of turn.",
        )
        .with_mana_cost(blue_cost(5, 1))
        .id();
    let next = scenario
        .add_spell_to_hand_from_oracle(P0, "Second priced instant", true, "You gain 1 life.")
        .with_mana_cost(blue_cost(5, 1))
        .id();
    let member = scenario
        .add_spell_to_graveyard(P0, "First priced instant", true)
        .id();
    scenario.add_spell_to_graveyard(P0, "First priced instant", true);
    scenario.add_spell_to_graveyard(P0, "Second priced instant", true);
    for name in [
        "Unrelated stack name",
        "Unrelated target name",
        "Locket of Yesterdays",
    ] {
        for _ in 0..3 {
            scenario.add_spell_to_graveyard(P0, name, true);
        }
    }
    let mut runner = scenario.build();
    runner.cast(unrelated).commit();
    assert_eq!(locket_count(&runner, source, first), 2);
    let outcome = runner.cast(first).target_object(target).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 5);
    assert_eq!(outcome.state().objects[&target].power, Some(3));
    outcome.assert_life_delta(P0, 2);
    assert_eq!(locket_count(&runner, source, next), 1);
    let outcome = runner.cast(next).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    // No explicit cast context: the same positive board cannot borrow a source,
    // chosen target, stack event, or LKI authority, even under inversion.
    let StaticMode::ModifyCost {
        dynamic_count: Some(qty),
        ..
    } = locket_definition().mode
    else {
        unreachable!()
    };
    assert_eq!(
        resolve_quantity(runner.state(), &QuantityExpr::Ref { qty }, P0, source),
        0
    );
    for relation in [
        SharedQualityRelation::Shares,
        SharedQualityRelation::DoesNotShare,
    ] {
        let filter =
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::Name,
                    reference: Some(Box::new(TargetFilter::CastingSpell)),
                    relation,
                }]),
            );
        let absent = FilterContext::from_source_with_controller(source, P0);
        assert!(!matches_target_filter(
            runner.state(),
            member,
            &filter,
            &absent
        ));
        let missing = absent.with_casting_spell(CastSpellContext {
            object_id: ObjectId(u64::MAX),
            casting_variant: CastingVariant::Normal,
        });
        assert!(!matches_target_filter(
            runner.state(),
            member,
            &filter,
            &missing
        ));
        let bound = absent.with_casting_spell(CastSpellContext {
            object_id: first,
            casting_variant: CastingVariant::Normal,
        });
        assert_eq!(
            matches_target_filter(runner.state(), member, &filter, &bound),
            relation == SharedQualityRelation::Shares
        );
    }
}

#[test]
fn locket_graveyard_announcement_and_lock() {
    use engine::types::ability::{CardPlayMode, StaticDefinition};
    use engine::types::statics::{CastFrequency, GraveyardPermissionPool};
    for cancel in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 1);
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_graveyard(P0, "Synthetic locked instant", true)
            .with_mana_cost(blue_cost(5, 1))
            .from_oracle_text("You gain 1 life.")
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic locked instant", true);
        let mana_source = scenario
            .add_artifact_from_oracle(
                P0,
                "Synthetic locked instant",
                "Sacrifice this artifact: Add {C}{C}{C}{C}.",
            )
            .id();
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic graveyard casting", "")
            .with_static_definition(
                StaticDefinition::new(StaticMode::GraveyardCastPermission {
                    frequency: CastFrequency::Unlimited,
                    play_mode: CardPlayMode::Cast,
                    graveyard_destination_replacement: None,
                    extra_cost: None,
                    enters_with_counter: None,
                    required_cast_keyword: None,
                    pool: GraveyardPermissionPool::OwnGraveyard,
                })
                .affected(TargetFilter::Typed(TypedFilter::card())),
            );
        let mut runner = scenario.build();
        assert_eq!(
            locket_count(&runner, source, spell),
            2,
            "in-origin preview includes the unannounced card"
        );
        begin_manual(&mut runner, spell);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        assert_eq!(locket_count(&runner, source, spell), 1);
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            blue_cost(4, 1)
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: mana_source,
                ability_index: 0,
            })
            .unwrap();
        assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
        assert_eq!(locket_count(&runner, source, spell), 2);
        // CR 601.2f: the newly arrived matching card cannot change the locked total.
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            blue_cost(4, 1)
        );
        if cancel {
            runner.act(GameAction::CancelCast).unwrap();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert!(runner.state().objects[&spell].spell_announcement.is_none());
            assert!(!runner.state().stack.iter().any(|entry| entry.id == spell));
            assert_eq!(locket_count(&runner, source, spell), 3);
            runner.state_mut().players[0].mana_pool = (0..4)
                .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                .collect();
            let outcome = runner.cast(spell).resolve();
            outcome.assert_zone(&[spell], Zone::Graveyard);
            outcome.assert_life_delta(P0, 1);
            assert_eq!(
                outcome.mana_pool_total(P0),
                0,
                "retry binds the new two-card population"
            );
        } else {
            runner.act(GameAction::PassPriority).unwrap();
            assert!(runner.state().pending_cast.is_none());
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().objects[&spell].mana_spent_to_cast_amount, 5);
            let before = runner.state().players[0].life;
            runner.resolve_top();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[0].life, before + 1);
        }
    }
}

#[test]
fn cast_name_population_modifier_sources() {
    for source_kind in ["self", "battlefield", "temporary"] {
        for matching in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            mana(&mut scenario, if matching { 4 } else { 6 });
            let source = if source_kind == "battlefield" {
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic independent dynamic source", "")
                    .with_static_definition(locket_definition())
                    .id()
            } else {
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic independent dynamic source", "")
                    .id()
            };
            let oracle = if source_kind == "self" {
                "This spell costs {1} less to cast for each card with the same name as that spell in your graveyard.\nYou gain 1 life."
            } else {
                "You gain 1 life."
            };
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, "Synthetic dynamic spell", true, oracle)
                .with_mana_cost(blue_cost(5, 1))
                .id();
            for _ in 0..2 {
                scenario.add_spell_to_graveyard(
                    P0,
                    if matching {
                        "Synthetic dynamic spell"
                    } else {
                        "Synthetic independent dynamic source"
                    },
                    true,
                );
            }
            let definition = locket_definition();
            let mut runner = scenario.build();
            match source_kind {
                "self" => {
                    let statics = &runner.state().objects[&spell].static_definitions;
                    assert_eq!(statics.len(), 1);
                    assert!(matches!(
                        &statics[0].mode,
                        StaticMode::ModifyCost {
                            dynamic_count: Some(QuantityRef::ObjectCount { .. }),
                            ..
                        }
                    ));
                }
                "battlefield" => {
                    assert_eq!(runner.state().objects[&source].static_definitions.len(), 1);
                }
                "temporary" => {
                    runner
                        .state_mut()
                        .add_transient_continuous_effect(
                            source,
                            P0,
                            Duration::UntilEndOfTurn,
                            TargetFilter::SpecificPlayer { id: P0 },
                            vec![ContinuousModification::GrantStaticAbility {
                                definition: Box::new(definition),
                            }],
                            None,
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            engine::game::layers::flush_layers(runner.state_mut());
            assert_eq!(
                locket_count(&runner, source, spell),
                if matching { 2 } else { 0 }
            );
            assert_eq!(
                engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
                Some(blue_cost(if matching { 3 } else { 5 }, 1)),
                "{source_kind}, matching={matching}"
            );
            let outcome = runner.cast(spell).resolve();
            outcome.assert_zone(&[spell], Zone::Graveyard);
            outcome.assert_life_delta(P0, 1);
            assert_eq!(
                outcome.mana_pool_total(P0),
                0,
                "{source_kind}, matching={matching}"
            );
        }
    }
}

#[test]
fn locket_foretell_face_up_name() {
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    for matching in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, 2);
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_hand(P0, "Behold the Multiverse", true)
            .with_mana_cost(blue_cost(3, 1))
            .from_oracle_text_with_keywords(&["Foretell", "Scry"], BEHOLD)
            .id();
        scenario.add_spell_to_graveyard(
            P0,
            if matching {
                "Behold the Multiverse"
            } else {
                "Other name"
            },
            true,
        );
        for _ in 0..5 {
            scenario.add_spell_to_library_top(P0, "Synthetic draw marker", true);
        }
        let mut runner = scenario.build();
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        assert!(runner.state().objects[&spell]
            .keywords
            .contains(&Keyword::Foretell(blue_cost(1, 1))));
        assert_eq!(runner.state().objects[&spell].abilities.len(), 1);
        assert!(!matches!(
            runner.state().objects[&spell].abilities[0].effect.as_ref(),
            Effect::Unimplemented { .. }
        ));
        runner
            .act(GameAction::Foretell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
            })
            .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        let object = &runner.state().objects[&spell];
        assert_eq!(object.zone, Zone::Exile);
        assert!(object.face_down && object.foretold);
        assert_eq!(object.name, "Behold the Multiverse");
        assert!(object.effective_names().is_empty());
        assert_eq!(
            object.spell_names_for(CastingVariant::Foretell),
            vec!["behold the multiverse"]
        );
        // Existing Foretell fixture recipe: the permission tests the recorded turn.
        runner.state_mut().turn_number += 1;
        runner.state_mut().players[0].mana_pool = (0..if matching { 1 } else { 2 })
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect();
        assert_eq!(locket_count(&runner, source, spell), i32::from(matching));
        let committed = runner
            .cast(spell)
            .casting_variant(CastingVariant::Foretell)
            .commit();
        let entry = committed
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == spell)
            .unwrap();
        assert!(
            matches!(&entry.kind, StackEntryKind::Spell { casting_variant: CastingVariant::Foretell, actual_mana_spent, .. } if *actual_mana_spent == if matching { 1 } else { 2 })
        );
        assert!(!committed.state().objects[&spell].spell_is_cast_face_down());
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_hand_drawn(P0, 2);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
}

#[test]
fn locket_concealed_exile_ordinary_cost_name() {
    use engine::types::ability::{CastingPermission, ExileGrantCostProvenance};
    use engine::types::game_state::CastingVariant;
    for matching in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, if matching { 3 } else { 4 });
        let source = add_locket(&mut scenario);
        let spell = scenario
            .add_spell_to_exile(P0, "Behold the Multiverse", true)
            .with_mana_cost(blue_cost(3, 1))
            .from_oracle_text_with_keywords(&["Foretell", "Scry"], BEHOLD)
            .id();
        scenario.add_spell_to_graveyard(
            P0,
            if matching {
                "Behold the Multiverse"
            } else {
                "Other name"
            },
            true,
        );
        for _ in 0..5 {
            scenario.add_spell_to_library_top(P0, "Synthetic ordinary draw marker", true);
        }
        let mut runner = scenario.build();
        let mut permission = alternative_permission(P0, 3, 0);
        let CastingPermission::ExileWithAltCost {
            cost,
            cost_provenance,
            cast_cost_modifier,
            ..
        } = &mut permission
        else {
            unreachable!()
        };
        *cost = blue_cost(3, 1);
        *cost_provenance = ExileGrantCostProvenance::NormalCost;
        *cast_cost_modifier = None;
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        object.face_down = true;
        object.casting_permissions = vec![permission];
        assert_eq!(
            object.spell_names_for(CastingVariant::Normal),
            vec!["behold the multiverse"]
        );
        assert_eq!(locket_count(&runner, source, spell), i32::from(matching));
        let committed = runner.cast(spell).commit();
        assert!(!committed.state().objects[&spell].spell_is_cast_face_down());
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_hand_drawn(P0, 2);
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
}

fn blue_cost(generic: u32, blue: usize) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::Blue; blue],
    }
}

fn mana(scenario: &mut GameScenario, count: usize) {
    scenario.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn sailors_board(populated: bool, pool: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, pool);
    let spell = scenario
        .add_creature_to_hand(P0, "Sailors' Bane", 7, 7)
        .with_mana_cost(blue_cost(7, 2))
        .from_oracle_text_with_keywords(&["Ward"], SAILORS)
        .id();
    let mut adventure_ids = Vec::new();
    if populated {
        let overlapping = scenario
            .add_spell_to_exile(P0, "Synthetic instant with Adventure", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Synthetic sorcery", false);
        let adventure = scenario
            .add_creature_to_graveyard(P0, "Synthetic Adventure permanent", 1, 1)
            .id();
        // CR 715.2a + CR 715.2c: actual alternative-face metadata qualifies a
        // permanent; the instant with the same metadata still counts only once.
        adventure_ids.extend([overlapping, adventure]);
        scenario.add_spell_to_exile(P1, "Wrong owner exile", true);
        scenario.add_spell_to_graveyard(P1, "Opponent graveyard", false);
        scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
        scenario.add_spell_to_hand(P0, "Wrong zone instant", true);
    }
    let mut runner = scenario.build();
    for id in adventure_ids {
        runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
            name: "Synthetic Adventure alternative".into(),
            layout_kind: Some(LayoutKind::Adventure),
            ..Default::default()
        });
    }
    (runner, spell)
}

#[test]
fn sailors_population_cost() {
    for (populated, pool) in [(true, 6), (false, 9)] {
        let (mut runner, spell) = sailors_board(populated, pool);
        assert_eq!(runner.state().objects[&spell].static_definitions.len(), 1);
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        let parsed = engine::parser::parse_oracle_text(
            SAILORS,
            "Sailors' Bane",
            &["Ward".into()],
            &["Creature".into()],
            &["Dragon".into(), "Turtle".into()],
        );
        let StaticMode::ModifyCost {
            dynamic_count: Some(quantity),
            ..
        } = &parsed.statics[0].mode
        else {
            panic!("full public coordinated population");
        };
        assert_eq!(
            resolve_quantity(
                runner.state(),
                &QuantityExpr::Ref {
                    qty: quantity.clone()
                },
                P0,
                spell
            ),
            if populated { 3 } else { 0 },
            "instant plus Adventure is one shared-population member"
        );
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        // CR 118.7a + CR 601.2f: three qualifying cards remove three generic
        // mana, retaining both blue pips; all hostile cards contribute zero.
        assert_eq!(outcome.mana_pool_total(P0), 0);
    }
    let (mut runner, spell) = sailors_board(true, 6);
    runner.state_mut().players[0].mana_pool = (0..6)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    // CR 118.7a: the generic reduction cannot pay either blue pip.
    assert!(runner.cast(spell).try_resolve().is_err());
    let (mut runner, spell) = sailors_board(true, 5);
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "one short cannot pay the locked cost"
    );
}

const CANDLEKEEP: &str = "Until end of turn, creatures you control have base power and toughness X/X, where X is the number of cards you own in exile and in your graveyard that are instant cards, are sorcery cards, and/or have an Adventure.";

fn candlekeep_board(
    populated: bool,
    recipients: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let spell = scenario
        .add_spell_to_hand(P0, "Candlekeep Inspiration", false)
        .with_mana_cost(blue_cost(4, 1))
        .from_oracle_text(CANDLEKEEP)
        .id();
    let mut controlled = Vec::new();
    if recipients {
        controlled.push(scenario.add_creature(P0, "Own recipient", 6, 7).id());
        controlled.push(
            scenario
                .add_creature(P0, "Foreign-owned recipient", 4, 5)
                .id(),
        );
    }
    let opponent = scenario.add_creature(P1, "Opponent recipient", 8, 9).id();
    let own_opponent = scenario
        .add_creature(P1, "Own opponent-controlled", 10, 11)
        .id();
    let late = scenario.add_creature_to_hand(P0, "Late entrant", 5, 6).id();
    let mut adventure_ids = Vec::new();
    if populated {
        let overlap = scenario
            .add_spell_to_exile(P0, "Instant Adventure", true)
            .id();
        scenario.add_spell_to_graveyard(P0, "Sorcery member", false);
        let adventure = scenario
            .add_creature_to_graveyard(P0, "Adventure permanent", 1, 1)
            .id();
        adventure_ids.extend([overlap, adventure]);
        scenario.add_spell_to_exile(P1, "Foreign exile", true);
        scenario.add_spell_to_graveyard(P1, "Opponent graveyard", false);
        scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
        scenario.add_spell_to_hand(P0, "Hand instant", true);
    }
    let mut runner = scenario.build();
    for id in adventure_ids {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.controller = P1;
        object.back_face = Some(BackFaceData {
            name: "Adventure alternative".into(),
            layout_kind: Some(LayoutKind::Adventure),
            ..Default::default()
        });
    }
    if recipients {
        runner
            .state_mut()
            .objects
            .get_mut(&controlled[1])
            .unwrap()
            .owner = P1;
    }
    runner
        .state_mut()
        .objects
        .get_mut(&own_opponent)
        .unwrap()
        .owner = P0;
    (runner, spell, controlled, opponent, late)
}

#[test]
fn candlekeep_population_base_pt() {
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(true, true);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    // CR 608.2h + CR 613.4b: the complete three-member population sets both
    // base characteristics when this sorcery resolves, before it enters the graveyard.
    for id in controlled {
        let obj = &outcome.state().objects[&id];
        assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
    }
    let obj = &outcome.state().objects[&opponent];
    assert_eq!((obj.power, obj.toughness), (Some(8), Some(9)));
    let other = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Own opponent-controlled")
        .unwrap();
    assert_eq!((other.power, other.toughness), (Some(10), Some(11)));
}

#[test]
fn candlekeep_population_resolution_binding() {
    let (mut runner, spell, controlled, _, late) = candlekeep_board(true, true);
    runner
        .cast(spell)
        .resolve()
        .assert_zone(&[spell], Zone::Graveyard);
    let assert_three = |runner: &GameRunner| {
        for id in &controlled {
            let obj = &runner.state().objects[id];
            assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
        }
    };
    // CR 611.2d: the source becoming a fourth graveyard member cannot update X.
    assert_three(&runner);
    let hand_instant = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Hand instant")
        .unwrap()
        .id;
    engine::game::zones::remove_from_zone(runner.state_mut(), hand_instant, Zone::Hand, P0);
    runner
        .state_mut()
        .objects
        .get_mut(&hand_instant)
        .unwrap()
        .zone = Zone::Graveyard;
    engine::game::zones::add_to_zone(runner.state_mut(), hand_instant, Zone::Graveyard, P0);
    engine::game::zones::remove_from_zone(runner.state_mut(), late, Zone::Hand, P0);
    runner.state_mut().objects.get_mut(&late).unwrap().zone = Zone::Battlefield;
    engine::game::zones::add_to_zone(runner.state_mut(), late, Zone::Battlefield, P0);
    {
        let recipient = runner.state_mut().objects.get_mut(&controlled[0]).unwrap();
        recipient.controller = P1;
        recipient.base_controller = Some(P1);
    }
    engine::game::layers::evaluate_layers(runner.state_mut());
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    // CR 611.2c: resolution fixes the recipients even after entry/control changes.
    assert_three(&runner);
    assert_eq!(runner.state().objects[&controlled[0]].controller, P1);
    let obj = &runner.state().objects[&late];
    assert_eq!((obj.power, obj.toughness), (Some(5), Some(6)));
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    // CR 514.2: real cleanup expires the effect and restores printed values.
    let obj = &runner.state().objects[&controlled[0]];
    assert_eq!((obj.power, obj.toughness), (Some(6), Some(7)));
    let obj = &runner.state().objects[&controlled[1]];
    assert_eq!((obj.power, obj.toughness), (Some(4), Some(5)));
}

#[test]
fn candlekeep_population_empty_cases() {
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(false, true);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    // CR 704.5f: zero base toughness is followed by the ordinary state-based death.
    outcome.assert_zone(&controlled, Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&opponent].power, Some(8));
    let (mut runner, spell, controlled, opponent, _) = candlekeep_board(true, false);
    assert!(controlled.is_empty());
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&opponent].toughness, Some(9));
}

#[test]
fn coordinated_population_draw_composition() {
    // Synthetic Sorcery witnesses: these compose the repaired population with
    // existing Draw/Offset/Multiply consumers, without replacing printed Oracle.
    for (introduction, populated_draws, empty_draws) in [
        ("the number of ", 3, 0),
        ("the total number of ", 3, 0),
        ("one plus the total number of ", 4, 1),
        ("twice the number of ", 6, 0),
    ] {
        for (populated, expected) in [(true, populated_draws), (false, empty_draws)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            mana(&mut scenario, 2);
            let source = format!("Draw X cards, where X is {introduction}cards you own in exile and in your graveyard that are instant cards, are sorcery cards, and/or have an Adventure.");
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic population Draw", false)
                .with_mana_cost(blue_cost(1, 1))
                .from_oracle_text(&source)
                .id();
            // Known qualifying types in Hand and Library must contribute zero.
            scenario.add_spell_to_hand(P0, "Hand instant", true);
            scenario.add_spell_to_hand(P1, "Opponent hand sorcery", false);
            for player in [P0, P1] {
                for index in 0..8 {
                    scenario.add_spell_to_library_top(
                        player,
                        &format!("Known library instant {index}"),
                        true,
                    );
                }
            }
            let mut adventure_ids = Vec::new();
            let mut foreign_ids = Vec::new();
            if populated {
                let overlap = scenario
                    .add_spell_to_exile(P0, "Instant Adventure member", true)
                    .id();
                let sorcery = scenario
                    .add_spell_to_graveyard(P0, "Sorcery member", false)
                    .id();
                let adventure = scenario
                    .add_creature_to_graveyard(P0, "Adventure permanent member", 1, 1)
                    .id();
                adventure_ids.extend([overlap, adventure]);
                foreign_ids.push(
                    scenario
                        .add_spell_to_exile(P1, "Foreign exile instant", true)
                        .id(),
                );
                foreign_ids.push(
                    scenario
                        .add_spell_to_graveyard(P1, "Opponent graveyard sorcery", false)
                        .id(),
                );
                scenario.add_creature_to_graveyard(P0, "Ordinary permanent", 1, 1);
                let mut runner = scenario.build();
                // CR 108.3 + CR 109.4: off-zone controller values cannot replace
                // ownership; hostile foreign cards coexist with positive members.
                for id in [overlap, sorcery, adventure] {
                    runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
                }
                for id in foreign_ids {
                    runner.state_mut().objects.get_mut(&id).unwrap().controller = P0;
                }
                // CR 715.2a + CR 715.2c: actual Adventure metadata qualifies the
                // permanent; Instant plus Adventure is still one card.
                for id in adventure_ids {
                    runner.state_mut().objects.get_mut(&id).unwrap().back_face =
                        Some(BackFaceData {
                            name: "Adventure alternative".into(),
                            layout_kind: Some(LayoutKind::Adventure),
                            ..Default::default()
                        });
                }
                assert_population_draw(&mut runner, spell, expected);
            } else {
                assert_population_draw(&mut scenario.build(), spell, expected);
            }
        }
    }
}

fn assert_population_draw(runner: &mut GameRunner, spell: ObjectId, expected: i64) {
    let outcome = runner.cast(spell).resolve();
    // CR 121.1 + CR 121.2: measure actual library-to-hand deliveries from stack
    // commit. CR 608.2h: the source is on Stack at the one population read;
    // its subsequent graveyard arrival cannot add another draw.
    outcome.assert_hand_drawn(P0, expected);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn coordinated_population_export_string_loader() {
    const BEACON: &str = "Beacon Bolt deals damage to target creature equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nJump-start (You may cast this card from your graveyard by discarding a card in addition to paying its other costs. Then exile this card.)";
    const DRAKE: &str = "Flying\nCrackling Drake's power is equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nWhen this creature enters, draw a card.";
    const HUSKBURSTER: &str = "This spell costs {1} less to cast for each creature card you own in exile and in your graveyard.\nMenace, deathtouch";
    const RAL: &str = "[+1]: Look at the top two cards of your library. Put one of them into your hand and the other into your graveyard.\n[−3]: Ral deals damage to target creature equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\n[−8]: You get an emblem with \"Whenever you cast an instant or sorcery spell, this emblem deals 4 damage to any target and you draw two cards.\"";
    const CURVE: &str = "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is one plus the total number of instant and sorcery cards you own in exile and in your graveyard.";

    let client_export =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    assert!(
        client_export.exists(),
        "generated client export is required"
    );
    let raw = shared_card_export_json().expect(
        "generated full export is required for coordinated population string-loader regression",
    );
    let mut entries = Map::<String, Value>::new();
    for (key, name, oracle) in [
        ("sailors' bane", "Sailors' Bane", SAILORS),
        (
            "candlekeep inspiration",
            "Candlekeep Inspiration",
            CANDLEKEEP,
        ),
        ("beacon bolt", "Beacon Bolt", BEACON),
        ("crackling drake", "Crackling Drake", DRAKE),
        ("huskburster swarm", "Huskburster Swarm", HUSKBURSTER),
        ("ral, izzet viceroy", "Ral, Izzet Viceroy", RAL),
        ("serpentine curve", "Serpentine Curve", CURVE),
    ] {
        let entry = raw.get(key).expect("required changed face in full export");
        assert_eq!(entry.get("name").and_then(Value::as_str), Some(name));
        assert_eq!(
            entry.get("oracle_text").and_then(Value::as_str),
            Some(oracle)
        );
        entries.insert(key.into(), entry.clone());
    }
    assert_eq!(entries.len(), 7);
    let serialized = to_string(&entries).expect("unchanged raw entries serialize");
    let db = CardDatabase::from_json_str(&serialized).expect("real changed export entries load");
    assert_eq!(db.face_iter().count(), 7);
    let sailors = db
        .get_face_by_name("Sailors' Bane")
        .expect("loaded Sailors' Bane");
    let candlekeep = db
        .get_face_by_name("Candlekeep Inspiration")
        .expect("loaded Candlekeep Inspiration");
    let beacon = db
        .get_face_by_name("Beacon Bolt")
        .expect("loaded Beacon Bolt");
    let drake = db
        .get_face_by_name("Crackling Drake")
        .expect("loaded Crackling Drake");
    let huskburster = db
        .get_face_by_name("Huskburster Swarm")
        .expect("loaded Huskburster Swarm");
    let ral = db
        .get_face_by_name("Ral, Izzet Viceroy")
        .expect("loaded Ral, Izzet Viceroy");
    let curve = db
        .get_face_by_name("Serpentine Curve")
        .expect("loaded Serpentine Curve");
    for (face, name, oracle) in [
        (sailors, "Sailors' Bane", SAILORS),
        (candlekeep, "Candlekeep Inspiration", CANDLEKEEP),
        (beacon, "Beacon Bolt", BEACON),
        (drake, "Crackling Drake", DRAKE),
        (huskburster, "Huskburster Swarm", HUSKBURSTER),
        (ral, "Ral, Izzet Viceroy", RAL),
        (curve, "Serpentine Curve", CURVE),
    ] {
        assert_eq!(face.name, name);
        assert_eq!(face.oracle_text.as_deref(), Some(oracle));
    }

    // CR 108.3 + CR 109.2a + CR 109.5 + CR 400.1: the spell's "you"
    // selects ownership and the stated zones, independently of stored controller.
    let owned = FilterProp::Owned {
        controller: ControllerRef::You,
    };
    let typed =
        |kind, properties| TargetFilter::Typed(TypedFilter::new(kind).properties(properties));
    let zones = TargetFilter::Or {
        filters: vec![
            typed(
                TypeFilter::Card,
                vec![FilterProp::InZone { zone: Zone::Exile }],
            ),
            typed(
                TypeFilter::Card,
                vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                    owned.clone(),
                ],
            ),
        ],
    };
    let qa = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    typed(TypeFilter::Card, vec![owned.clone()]),
                    zones.clone(),
                    TargetFilter::Or {
                        filters: vec![
                            typed(TypeFilter::Instant, vec![]),
                            typed(TypeFilter::Sorcery, vec![]),
                            typed(TypeFilter::Card, vec![FilterProp::HasAdventure]),
                        ],
                    },
                ],
            },
        },
    };
    let qi = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Or {
                        filters: vec![
                            typed(TypeFilter::Instant, vec![owned.clone()]),
                            typed(TypeFilter::Sorcery, vec![owned.clone()]),
                        ],
                    },
                    zones.clone(),
                ],
            },
        },
    };
    let qc = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::And {
                filters: vec![typed(TypeFilter::Creature, vec![owned]), zones],
            },
        },
    };

    let sailors_static = sailors
        .static_abilities
        .first()
        .expect("Sailors cost static");
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(count),
        ..
    } = &sailors_static.mode
    else {
        panic!("Sailors dynamic cost slot");
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert_eq!(sailors_static.affected, Some(TargetFilter::SelfRef));
    let sailors_count = QuantityExpr::Ref { qty: count.clone() };
    assert_eq!(sailors_count, qa);

    let candlekeep_ability = candlekeep
        .abilities
        .first()
        .expect("Candlekeep spell ability");
    let Effect::GenericEffect {
        static_abilities,
        duration,
        ..
    } = candlekeep_ability.effect.as_ref()
    else {
        panic!("Candlekeep continuous effect slot");
    };
    // CR 611.2a + CR 613.4b: both base characteristics have the printed duration.
    assert_eq!(candlekeep_ability.duration, Some(Duration::UntilEndOfTurn));
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    let [candlekeep_static] = static_abilities.as_slice() else {
        panic!("one Candlekeep static");
    };
    assert_eq!(candlekeep_static.mode, StaticMode::Continuous);
    assert_eq!(
        candlekeep_static.affected,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    assert!(!candlekeep_static.characteristic_defining);
    let [ContinuousModification::SetPowerDynamic {
        value: candlekeep_power,
    }, ContinuousModification::SetToughnessDynamic {
        value: candlekeep_toughness,
    }] = candlekeep_static.modifications.as_slice()
    else {
        panic!("exactly one distinct Candlekeep power and toughness slot");
    };
    assert_eq!(candlekeep_power, &qa);
    assert_eq!(candlekeep_toughness, &qa);

    let Effect::DealDamage {
        amount: beacon_count,
        ..
    } = beacon
        .abilities
        .first()
        .expect("Beacon ability")
        .effect
        .as_ref()
    else {
        panic!("Beacon damage amount slot");
    };
    assert_eq!(beacon_count, &qi);
    let drake_static = drake.static_abilities.first().expect("Drake power static");
    assert_eq!(drake_static.mode, StaticMode::Continuous);
    assert!(drake_static.characteristic_defining);
    assert_eq!(drake_static.affected, Some(TargetFilter::SelfRef));
    let [ContinuousModification::SetDynamicPower { value: drake_count }] =
        drake_static.modifications.as_slice()
    else {
        panic!("exactly one Drake dynamic power slot");
    };
    assert_eq!(drake_count, &qi);
    let huskburster_static = huskburster
        .static_abilities
        .first()
        .expect("Huskburster cost static");
    let StaticMode::ModifyCost {
        mode,
        amount,
        dynamic_count: Some(count),
        ..
    } = &huskburster_static.mode
    else {
        panic!("Huskburster dynamic cost slot");
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(*amount, ManaCost::generic(1));
    assert_eq!(huskburster_static.affected, Some(TargetFilter::SelfRef));
    let huskburster_count = QuantityExpr::Ref { qty: count.clone() };
    assert_eq!(huskburster_count, qc);
    let ral_ability = ral.abilities.get(1).expect("Ral minus-three ability");
    assert_eq!(ral_ability.cost, Some(AbilityCost::Loyalty { amount: -3 }));
    let Effect::DealDamage {
        amount: ral_count, ..
    } = ral_ability.effect.as_ref()
    else {
        panic!("Ral minus-three damage amount slot");
    };
    assert_eq!(ral_count, &qi);
    let Effect::Token {
        enter_with_counters,
        ..
    } = curve
        .abilities
        .first()
        .expect("Curve token ability")
        .effect
        .as_ref()
    else {
        panic!("Curve token counter slot");
    };
    let [(CounterType::Plus1Plus1, curve_count)] = enter_with_counters.as_slice() else {
        panic!("one Curve +1/+1 counter quantity");
    };
    assert_eq!(
        curve_count,
        &QuantityExpr::Offset {
            inner: Box::new(qi),
            offset: 1
        }
    );

    let slots = [
        ("Sailors cost", &sailors_count, 3, 0),
        ("Candlekeep power", candlekeep_power, 3, 0),
        ("Candlekeep toughness", candlekeep_toughness, 3, 0),
        ("Beacon damage", beacon_count, 2, 0),
        ("Drake power", drake_count, 2, 0),
        ("Huskburster cost", &huskburster_count, 2, 0),
        ("Ral damage", ral_count, 2, 0),
        ("Curve counters", curve_count, 3, 1),
    ];
    assert_eq!(slots.len(), 8);
    for populated in [true, false] {
        let mut scenario = GameScenario::new();
        let source = scenario
            .add_artifact_from_oracle(P0, "Loaded count source", "")
            .id();
        let mut owned_ids = Vec::new();
        let mut adventure_ids = Vec::new();
        if populated {
            let overlap = scenario
                .add_spell_to_exile(P0, "Loaded Instant Adventure", true)
                .id();
            let sorcery = scenario
                .add_spell_to_graveyard(P0, "Loaded Sorcery member", false)
                .id();
            let adventure = scenario
                .add_creature_to_graveyard(P0, "Loaded Adventure creature", 1, 1)
                .id();
            let ordinary = scenario
                .add_creature_to_graveyard(P0, "Loaded ordinary creature", 1, 1)
                .id();
            owned_ids.extend([overlap, sorcery, adventure, ordinary]);
            adventure_ids.extend([overlap, adventure]);
        }
        let foreign_ids = [
            scenario
                .add_spell_to_exile(P1, "Loaded foreign exile Instant", true)
                .id(),
            scenario
                .add_spell_to_graveyard(P1, "Loaded opponent Sorcery", false)
                .id(),
            scenario
                .add_creature_to_graveyard(P1, "Loaded opponent Creature", 1, 1)
                .id(),
        ];
        scenario.add_spell_to_hand(P0, "Loaded hand Instant", true);
        scenario.add_spell_to_library_top(P0, "Loaded library Instant", true);
        let mut runner = scenario.build();
        // CR 109.4: off-zone controller storage is not ownership authority.
        for id in owned_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
        }
        for id in foreign_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P0;
        }
        // CR 715.2a + CR 715.2c: real alternative characteristics qualify the
        // Adventure creature; the Instant plus Adventure remains one card.
        for id in adventure_ids {
            runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
                name: "Loaded Adventure alternative".into(),
                layout_kind: Some(LayoutKind::Adventure),
                ..Default::default()
            });
        }
        for (slot, quantity, positive, empty) in slots {
            assert_eq!(
                resolve_quantity(runner.state(), quantity, P0, source),
                if populated { positive } else { empty },
                "{slot}, populated={populated}"
            );
        }
    }
}

const DISMISSAL: &str = "This spell costs {1} less to cast for each artifact you control and each artifact card in your hand.\nCounter target noncreature spell.";
const REPEATED: &str = "This spell costs {1} less to cast for each artifact you control and each artifact card in your hand.";
const LEECH: &str = "This spell costs {1} less to cast for each Cave you control and each Cave card in your graveyard.\nLifelink";
#[test]
fn dismissal_population_and_counter() {
    for (populated, short) in [(true, false), (false, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(
            &mut scenario,
            if short {
                2
            } else if populated {
                3
            } else {
                5
            },
        );
        let selected = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic selected noncreature",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let other = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic other noncreature",
                true,
                "You gain 2 life.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let dismissal = scenario
            .add_spell_to_hand_from_oracle(P0, "Machinist's Dismissal", true, DISMISSAL)
            .with_mana_cost(blue_cost(4, 1))
            .id();
        if populated {
            // CR 109.4 + CR 109.5: the controlled leg follows controller, not owner.
            scenario
                .add_artifact_from_oracle(P1, "Stolen artifact", "")
                .controlled_by(P0);
            scenario.add_artifact_to_hand_from_oracle(P0, "Own hand artifact", "");
            scenario.add_artifact_from_oracle(P1, "Opponent artifact", "");
            scenario
                .add_artifact_from_oracle(P0, "Owned uncontrolled artifact", "")
                .controlled_by(P1);
            scenario.add_artifact_to_hand_from_oracle(P1, "Opponent hand artifact", "");
            scenario
                .add_creature_to_graveyard(P0, "Graveyard artifact", 1, 1)
                .as_artifact_creature();
            scenario.add_creature_to_hand(P0, "Hand nonartifact", 1, 1);
        }
        let mut runner = scenario.build();
        let mut first = runner.cast(selected).commit();
        let mut second = first.cast(other).commit();
        if short {
            assert!(second
                .cast(dismissal)
                .target_object(selected)
                .try_resolve()
                .is_err());
            continue;
        }
        let outcome = second.cast(dismissal).target_object(selected).resolve();
        outcome.assert_zone(&[selected, dismissal, other], Zone::Graveyard);
        // CR 601.2f: both scoped members reduce {4}U to {2}U.
        assert_eq!(outcome.mana_pool_total(P0), 0);
        // CR 608.2b: only the declared noncreature target is countered. The
        // other eligible spell resolves; the selected spell never gains life.
        outcome.assert_life_delta(P0, 2);
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, countered_by, .. } if *object_id == selected && *countered_by == dismissal)));
        assert!(!outcome.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == other)));
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let creature = scenario
        .add_creature_to_hand(P0, "Synthetic illegal creature spell target", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let dismissal = scenario
        .add_spell_to_hand_from_oracle(P0, "Machinist's Dismissal", true, DISMISSAL)
        .with_mana_cost(blue_cost(4, 1))
        .id();
    let mut runner = scenario.build();
    let mut committed = runner.cast(creature).commit();
    assert!(committed.cast(dismissal).try_resolve().is_err());
}

fn artifact_in(zone: Zone) -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::InZone { zone }]),
    )
}

fn count(state: &GameState, source: ObjectId, filter: TargetFilter) -> i32 {
    resolve_quantity(
        state,
        &QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        P0,
        source,
    )
}

#[test]
fn pending_artifact_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 2);
    let spell = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic pending population spell", REPEATED)
        .with_keyword(Keyword::Flash)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.add_artifact_from_oracle(P0, "Battlefield member", "");
    let hand = scenario
        .add_artifact_to_hand_from_oracle(P0, "Ordinary hand member", "")
        .id();
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unrelated nonartifact stack object",
            true,
            "You gain 1 life.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let _committed = runner.cast(unrelated).commit();
    let completed_state = runner.state().clone();
    let union = TargetFilter::Or {
        filters: vec![artifact_in(Zone::Battlefield), artifact_in(Zone::Hand)],
    };
    assert_eq!(count(runner.state(), spell, union.clone()), 3);
    let origin_snapshot = runner.state().objects[&spell].clone();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: origin_snapshot.card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    assert!(runner.state().objects[&spell].spell_announcement.is_some());
    assert!(runner.state().stack.iter().any(|entry| entry.id == spell));
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().object_id,
        spell
    );
    // CR 601.2a + CR 601.2f: pending is Stack, not Hand, when cost locks.
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(2)
    );
    assert_eq!(count(runner.state(), spell, union.clone()), 2);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 1);
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::And {
                filters: vec![
                    artifact_in(Zone::Hand),
                    TargetFilter::SpecificObject { id: spell }
                ]
            }
        ),
        0
    );
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::And {
                filters: vec![
                    artifact_in(Zone::Hand),
                    TargetFilter::SpecificObject { id: hand }
                ]
            }
        ),
        1
    );
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 1);
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::InAnyZone {
                    zones: vec![Zone::Hand, Zone::Stack]
                }
            ]))
        ),
        2
    );
    let nested = TargetFilter::And {
        filters: vec![
            TargetFilter::Or {
                filters: vec![artifact_in(Zone::Hand), artifact_in(Zone::Stack)],
            },
            TargetFilter::Not {
                filter: Box::new(artifact_in(Zone::Hand)),
            },
        ],
    };
    assert_eq!(count(runner.state(), spell, nested), 1);
    assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
    runner.act(GameAction::CancelCast).unwrap();
    assert_eq!(count(runner.state(), spell, union), 3);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let mut completed = GameRunner::from_state(completed_state);
    let committed = completed.cast(spell).commit();
    assert_eq!(count(committed.state(), spell, artifact_in(Zone::Stack)), 1);
    let outcome = committed.resolve();
    outcome.assert_zone(&[spell], Zone::Battlefield);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

#[test]
fn leech_population_cost() {
    for (populated, colorless, short) in [
        (true, false, false),
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            (0..if short {
                5
            } else if populated {
                6
            } else {
                8
            })
                .map(|_| {
                    ManaUnit::new(
                        if colorless {
                            ManaType::Colorless
                        } else {
                            ManaType::Black
                        },
                        ObjectId(0),
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let spell = scenario
            .add_creature_to_hand(P0, "Gargantuan Leech", 5, 5)
            .with_subtypes(vec!["Leech"])
            .with_mana_cost(ManaCost::Cost {
                generic: 7,
                shards: vec![ManaCostShard::Black],
            })
            .from_oracle_text_with_keywords(&["Lifelink"], LEECH)
            .id();
        let mut off_zone = None;
        if populated {
            scenario
                .add_land_from_oracle(P1, "Controlled foreign Cave", "")
                .with_subtypes(vec!["Cave"])
                .controlled_by(P0);
            off_zone = Some(
                scenario
                    .add_land_to_graveyard(P0, "Own graveyard Cave")
                    .with_subtypes(vec!["Cave"])
                    .id(),
            );
            scenario
                .add_land_to_graveyard(P1, "Opponent Cave")
                .with_subtypes(vec!["Cave"]);
            scenario
                .add_land_from_oracle(P0, "Owned uncontrolled Cave", "")
                .with_subtypes(vec!["Cave"])
                .controlled_by(P1);
            scenario
                .add_land_to_hand(P0, "Hand Cave")
                .with_subtypes(vec!["Cave"]);
            scenario.add_land_to_graveyard(P0, "Ordinary land");
        }
        let mut runner = scenario.build();
        if let Some(id) = off_zone {
            runner.state_mut().objects.get_mut(&id).unwrap().controller = P1;
        }
        assert_eq!(runner.state().objects[&spell].static_definitions.len(), 1);
        let StaticMode::ModifyCost {
            dynamic_count: Some(quantity),
            ..
        } = &runner.state().objects[&spell].static_definitions[0].mode
        else {
            panic!("complete printed Cave population");
        };
        assert_eq!(
            resolve_quantity(
                runner.state(),
                &QuantityExpr::Ref {
                    qty: quantity.clone()
                },
                P0,
                spell
            ),
            if populated { 2 } else { 0 },
            "battlefield control and graveyard ownership have separate authorities"
        );
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(ManaCost::Cost {
                generic: if populated { 5 } else { 7 },
                shards: vec![ManaCostShard::Black]
            })
        );
        if colorless || short {
            assert!(runner.cast(spell).try_resolve().is_err());
            continue;
        }
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[spell], Zone::Battlefield);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(outcome.state().objects[&spell]
            .keywords
            .contains(&Keyword::Lifelink));
    }
}

#[test]
fn independent_repeated_population_overlap() {
    // Synthetic expression witnesses: one object satisfies two independent each clauses.
    for second in ["creature", "artifact"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let member = scenario
            .add_creature(P0, "Artifact Creature", 2, 3)
            .as_artifact_creature()
            .id();
        let oracle =
            format!("Draw a card for each artifact you control and each {second} you control.");
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic independent Draw", false, &oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        for i in 0..5 {
            scenario.add_spell_to_library_top(P0, &format!("Draw member {i}"), true);
        }
        let mut runner = scenario.build();
        let parsed = engine::parser::parse_oracle_text(
            &oracle,
            "Synthetic independent Draw",
            &[],
            &["Sorcery".into()],
            &[],
        );
        assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
        let outcome = runner.cast(spell).resolve();
        outcome.assert_hand_drawn(P0, 2);
        assert_eq!(outcome.state().objects[&member].power, Some(2));
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let member = scenario
        .add_creature(P0, "Anthem Artifact Creature", 2, 3)
        .as_artifact_creature()
        .id();
    scenario.add_enchantment_from_oracle(P0, "Synthetic independent anthem", "Creatures you control get +1/+1 for each artifact you control and each creature you control.");
    let marker = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic flush", false, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(marker)
        .resolve()
        .assert_power_toughness(member, 4, 5);
}

#[test]
fn repeated_population_full_source_honesty() {
    for (text, name, types, keywords) in [
        (
            DISMISSAL,
            "Machinist's Dismissal",
            vec!["Instant".into()],
            vec![],
        ),
        (
            LEECH,
            "Gargantuan Leech",
            vec!["Creature".into()],
            vec!["Lifelink".into()],
        ),
    ] {
        let parsed = engine::parser::parse_oracle_text(text, name, &keywords, &types, &[]);
        assert_eq!(parsed.statics.len(), 1, "{parsed:#?}");
        assert!(
            matches!(&parsed.statics[0].mode, StaticMode::ModifyCost { dynamic_count: Some(QuantityRef::ObjectCount { filter: TargetFilter::Or { filters } }), .. } if filters.len() == 2)
        );
        assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
        if name == "Machinist's Dismissal" {
            assert_eq!(parsed.abilities.len(), 1);
        } else {
            assert!(parsed.extracted_keywords.contains(&Keyword::Lifelink));
        }
    }
    for body in [
        "artifact you control and each creature you control",
        "artifact you control and each artifact you control",
        "artifact you control and each mystery in your hand",
        "artifact you control and each artifact card in your hand and each",
        "artifact you control and each artifact card in your hand with unknown frosting",
        "card in exile and in your graveyard and each mystery you control",
        "artifact you control and eachness artifact card in your hand",
    ] {
        let text = format!("This spell costs {{1}} less to cast for each {body}.");
        let parsed = engine::parser::parse_oracle_text(
            &text,
            "Synthetic unsupported scalar",
            &[],
            &["Sorcery".into()],
            &[],
        );
        assert!(
            parsed.statics.is_empty(),
            "must not admit a partial/fixed discount: {body}: {parsed:#?}"
        );
        assert!(parsed.abilities.iter().any(|ability| matches!(ability.effect.as_ref(), Effect::Unimplemented { description: Some(description), .. } if description.trim_end_matches('.') == text.trim_end_matches('.') && ability.description.as_deref() == Some(text.as_str()))), "full source must remain visible: {parsed:#?}");
    }
}

#[test]
fn repeated_population_generalized_zone_families() {
    for (body, generic, expected) in [
        ("artifact card in your hand and each artifact you control", 4, 2),
        ("creature your opponents control and each creature card in your graveyard and each artifact card in your hand", 5, 2),
        ("card you own in exile and in your graveyard and each artifact you control", 5, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        mana(&mut scenario, expected);
        let text = format!("This spell costs {{1}} less to cast for each {body}.\nYou gain 1 life.");
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic disjoint family", false, &text).with_mana_cost(ManaCost::generic(generic)).id();
        scenario.add_artifact_from_oracle(P0, "Family battlefield artifact", "");
        scenario.add_artifact_to_hand_from_oracle(P0, "Family hand artifact", "");
        scenario.add_creature(P1, "Family opponent creature", 1, 1);
        scenario.add_creature_to_graveyard(P0, "Family graveyard creature", 1, 1);
        scenario.add_spell_to_exile(P0, "Family exiled instant", true);
        let mut runner = scenario.build();
        assert!(runner.state().objects[&spell].parse_warnings.is_empty());
        let outcome = runner.cast(spell).resolve();
        assert_eq!(outcome.mana_pool_total(P0), 0);
        outcome.assert_life_delta(P0, 1);
    }
}

fn begin_manual(runner: &mut GameRunner, spell: ObjectId) {
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    assert!(runner.state().objects[&spell].spell_announcement.is_some());
    assert!(runner.state().stack.iter().any(|entry| entry.id == spell));
}

#[test]
fn pending_population_lock_payment_and_cancel() {
    for real_ability in [false, true] {
        for has_mana_source in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let text = if real_ability {
                format!("{REPEATED}\nYou gain 1 life.")
            } else {
                REPEATED.into()
            };
            let spell = scenario
                .add_artifact_to_hand_from_oracle(P0, "Synthetic locked artifact", &text)
                .with_mana_cost(ManaCost::generic(4))
                .id();
            let source = scenario
                .add_artifact_from_oracle(
                    P0,
                    "Synthetic sacrificial mana artifact",
                    if has_mana_source {
                        "Sacrifice this artifact: Add {C}{C}."
                    } else {
                        ""
                    },
                )
                .id();
            scenario.add_artifact_to_hand_from_oracle(P0, "Remaining hand artifact", "");
            let mut runner = scenario.build();
            assert_eq!(
                runner.state().objects[&spell].abilities.is_empty(),
                !real_ability
            );
            begin_manual(&mut runner, spell);
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { .. }
            ));
            if has_mana_source {
                // Confirming payment may activate the available source. An empty
                // starting pool is not an insufficient-payment fixture in this case.
                let mut automatic = GameRunner::from_state(runner.state().clone());
                automatic.act(GameAction::PassPriority).unwrap();
                assert!(automatic.state().pending_cast.is_none());
                assert_eq!(automatic.state().objects[&source].zone, Zone::Graveyard);
                assert_eq!(
                    automatic.state().objects[&spell].mana_spent_to_cast_amount,
                    2
                );
                assert_eq!(automatic.state().players[0].mana_pool.total(), 0);
            } else {
                assert!(
                    runner.act(GameAction::PassPriority).is_err(),
                    "an empty pool with no mana ability cannot pay the locked cost"
                );
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ManaPayment { .. }
                ));
                assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
                assert_eq!(runner.state().objects[&spell].mana_spent_to_cast_amount, 0);
            }
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            runner.act(GameAction::CancelCast).unwrap();
            assert!(runner.state().pending_cast.is_none());
            assert!(runner.state().objects[&spell].spell_announcement.is_none());
            assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 2);
            assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
            if !has_mana_source {
                continue;
            }
            begin_manual(&mut runner, spell);
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(
                count(runner.state(), spell, artifact_in(Zone::Battlefield)),
                0
            );
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
            runner.act(GameAction::PassPriority).unwrap();
            assert!(runner.state().pending_cast.is_none());
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            runner.resolve_top();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
        }
    }
}

fn population_modifier(dynamic: bool) -> engine::types::ability::StaticDefinition {
    let parsed = engine::parser::parse_oracle_text(
        REPEATED,
        "Synthetic population source",
        &[],
        &["Artifact".into()],
        &[],
    );
    let mut definition = parsed
        .statics
        .into_iter()
        .next()
        .expect("complete population static");
    definition.affected = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::You),
    ));
    // CR 113.6: board and object-granted abilities use battlefield default.
    definition.active_zones.clear();
    if !dynamic {
        let StaticMode::ModifyCost { dynamic_count, .. } = &mut definition.mode else {
            unreachable!()
        };
        *dynamic_count = None;
    }
    definition
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplicabilitySource {
    SelfSpell,
    Battlefield,
    Command,
    ObjectGrant,
    TransientDefinition,
    TransientGate,
    TransientDuration,
}

fn applicability_fixture(
    source_kind: ApplicabilitySource,
    generic: u32,
    dynamic: bool,
    condition: Option<StaticCondition>,
    filter: Option<TargetFilter>,
) -> (GameRunner, ObjectId) {
    use engine::types::ability::StaticDefinition;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut definition = population_modifier(dynamic);
    let mut gate = None;
    let mut duration = Duration::UntilEndOfTurn;
    match source_kind {
        ApplicabilitySource::TransientGate => gate = condition,
        ApplicabilitySource::TransientDuration => {
            duration = Duration::ForAsLongAs {
                condition: condition.expect("duration needs its initial true condition"),
            };
        }
        ApplicabilitySource::SelfSpell
        | ApplicabilitySource::Battlefield
        | ApplicabilitySource::Command
        | ApplicabilitySource::ObjectGrant
        | ApplicabilitySource::TransientDefinition => definition.condition = condition,
    }
    let StaticMode::ModifyCost { spell_filter, .. } = &mut definition.mode else {
        unreachable!()
    };
    *spell_filter = filter;
    if source_kind == ApplicabilitySource::SelfSpell {
        definition.affected = Some(TargetFilter::SelfRef);
        definition.active_zones = engine::types::zones::self_spell_cost_mod_active_zones();
    } else if source_kind == ApplicabilitySource::Command {
        definition.active_zones = vec![Zone::Command];
    }
    let spell = if source_kind == ApplicabilitySource::SelfSpell {
        scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic applicability spell", "")
            .with_mana_cost(ManaCost::generic(generic))
            .with_static_definition(definition.clone())
            .id()
    } else {
        scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic applicability spell", "")
            .with_mana_cost(ManaCost::generic(generic))
            .id()
    };
    // The spell is the third hand artifact before announcement. Together
    // with the battlefield artifact it makes the repeated population four.
    scenario.add_artifact_to_hand_from_oracle(P0, "Applicability hand member one", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Applicability hand member two", "");
    scenario.add_artifact_from_oracle(P0, "Applicability battlefield member", "");
    let host = if matches!(
        source_kind,
        ApplicabilitySource::Battlefield | ApplicabilitySource::Command
    ) {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic applicability host", "")
            .with_static_definition(definition.clone())
            .id()
    } else {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic applicability host", "")
            .id()
    };
    if source_kind == ApplicabilitySource::ObjectGrant {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic applicability granter", "")
            .with_static_definition(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: host })
                    .modifications(vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(definition.clone()),
                    }]),
            );
    }
    let mut runner = scenario.build();
    if source_kind == ApplicabilitySource::Command {
        engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Command, &mut Vec::new());
    } else if matches!(
        source_kind,
        ApplicabilitySource::TransientDefinition
            | ApplicabilitySource::TransientGate
            | ApplicabilitySource::TransientDuration
    ) {
        let id = runner
            .state_mut()
            .add_transient_continuous_effect(
                host,
                P0,
                duration,
                TargetFilter::SpecificPlayer { id: P0 },
                vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(definition),
                }],
                gate,
            )
            .expect("the public producer installs this effect before announcement");
        assert!(runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| effect.id == id));
        assert!(runner.state().objects[&spell].static_definitions.is_empty());
    }
    // Settle the fixture's real layer-6 grants before reading a pure preview.
    engine::game::layers::flush_layers(runner.state_mut());
    (runner, spell)
}

fn assert_applicability_payment(
    mut runner: GameRunner,
    spell: ObjectId,
    preview: u32,
    locked: u32,
    source_kind: ApplicabilitySource,
) {
    let functioning_sources =
        engine::game::functioning_abilities::game_functioning_statics(runner.state())
            .filter(|(_, definition)| matches!(definition.mode, StaticMode::ModifyCost { .. }))
            .count();
    assert_eq!(
        functioning_sources,
        usize::from(matches!(
            source_kind,
            ApplicabilitySource::Battlefield
                | ApplicabilitySource::Command
                | ApplicabilitySource::ObjectGrant
        )),
        "{source_kind:?}: the intended object-source class must actually function"
    );
    assert_eq!(
        engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
        Some(ManaCost::generic(preview)),
        "{source_kind:?}: preview must reach the opposite predicate result"
    );
    begin_manual(&mut runner, spell);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 2);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 1);
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(locked),
        "{source_kind:?}: announcement changes applicability"
    );
    // CR 601.2f + CR 601.2h: pay the newly locked amount exactly; neither
    // a stale free preview nor a stale full-cost preview can satisfy this.
    runner.state_mut().players[0].mana_pool = (0..locked)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect();
    runner.act(GameAction::PassPriority).unwrap();
    assert!(runner.state().pending_cast.is_none(), "{source_kind:?}");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{source_kind:?}"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Battlefield,
        "{source_kind:?}"
    );
}

#[test]
fn pending_population_condition_applicability_crosses_announcement() {
    use engine::types::ability::Comparator;
    for source_kind in [
        ApplicabilitySource::SelfSpell,
        ApplicabilitySource::Battlefield,
        ApplicabilitySource::Command,
        ApplicabilitySource::ObjectGrant,
        ApplicabilitySource::TransientDefinition,
        ApplicabilitySource::TransientGate,
        ApplicabilitySource::TransientDuration,
    ] {
        for disappears in [true, false] {
            // A false initial ForAsLongAs duration must never start. Exercise
            // that source's actual true-to-false lifetime only.
            if source_kind == ApplicabilitySource::TransientDuration && !disappears {
                continue;
            }
            let condition = StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: artifact_in(Zone::Hand),
                    },
                },
                comparator: if disappears {
                    Comparator::GE
                } else {
                    Comparator::LE
                },
                rhs: QuantityExpr::Fixed {
                    value: if disappears { 3 } else { 2 },
                },
            };
            // Nested conditions use the same conservative applicability proof.
            let condition = StaticCondition::And {
                conditions: vec![StaticCondition::Not {
                    condition: Box::new(StaticCondition::Not {
                        condition: Box::new(condition),
                    }),
                }],
            };
            let (generic, preview, locked) = if disappears { (4, 0, 4) } else { (2, 2, 1) };
            let (runner, spell) =
                applicability_fixture(source_kind, generic, disappears, Some(condition), None);
            assert_applicability_payment(runner, spell, preview, locked, source_kind);
        }
    }
}

#[test]
fn pending_population_spell_filter_applicability_crosses_announcement() {
    use engine::types::ability::Comparator;
    for source_kind in [
        ApplicabilitySource::SelfSpell,
        ApplicabilitySource::Battlefield,
        ApplicabilitySource::Command,
        ApplicabilitySource::ObjectGrant,
        ApplicabilitySource::TransientDefinition,
    ] {
        for disappears in [true, false] {
            let definition = population_modifier(true);
            let StaticMode::ModifyCost {
                dynamic_count: Some(repeated),
                ..
            } = definition.mode
            else {
                unreachable!()
            };
            let threshold = if disappears {
                repeated
            } else {
                QuantityRef::ObjectCount {
                    filter: artifact_in(Zone::Hand),
                }
            };
            // Nested filter logic must not use today's false result to erase
            // yesterday's contribution, or to miss tomorrow's fixed reduction.
            let filter = TargetFilter::And {
                filters: vec![TargetFilter::Not {
                    filter: Box::new(TargetFilter::Not {
                        filter: Box::new(TargetFilter::Typed(TypedFilter::default().properties(
                            vec![FilterProp::Cmc {
                                comparator: if disappears {
                                    Comparator::LE
                                } else {
                                    Comparator::GE
                                },
                                value: QuantityExpr::Ref { qty: threshold },
                            }],
                        ))),
                    }),
                }],
            };
            let (generic, preview, locked) = if disappears { (4, 0, 4) } else { (2, 2, 1) };
            let (runner, spell) =
                applicability_fixture(source_kind, generic, disappears, None, Some(filter));
            assert_applicability_payment(runner, spell, preview, locked, source_kind);
        }
    }
}

#[test]
fn pending_population_transient_grant_lock() {
    for mode in ["dynamic", "fixed", "expired", "wrong caster", "wrong spell"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic transient-only artifact", "")
            .with_mana_cost(ManaCost::generic(4))
            .id();
        let source = scenario
            .add_artifact_from_oracle(
                P0,
                "Synthetic temporary grant source",
                "Sacrifice this artifact: Add {C}{C}.",
            )
            .id();
        scenario.add_artifact_to_hand_from_oracle(P0, "Transient hand member", "");
        let mut runner = scenario.build();
        let mut definition = population_modifier(mode != "fixed");
        if mode == "wrong spell" {
            let StaticMode::ModifyCost { spell_filter, .. } = &mut definition.mode else {
                unreachable!()
            };
            *spell_filter = Some(TargetFilter::Typed(TypedFilter::creature()));
        }
        let controller = if mode == "wrong caster" { P1 } else { P0 };
        let effect_id = runner
            .state_mut()
            .add_transient_continuous_effect(
                source,
                controller,
                Duration::UntilHostLeavesPlay,
                TargetFilter::SpecificPlayer { id: controller },
                vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(definition),
                }],
                None,
            )
            .unwrap();
        engine::game::layers::flush_layers(runner.state_mut());
        assert!(runner.state().transient_continuous_effects.iter().any(|effect| effect.id == effect_id && matches!(effect.affected, TargetFilter::SpecificPlayer { id } if id == controller)));
        assert!(
            !engine::game::functioning_abilities::game_functioning_statics(runner.state())
                .any(|(_, definition)| matches!(definition.mode, StaticMode::ModifyCost { .. }))
        );
        assert!(runner.state().objects[&spell].static_definitions.is_empty());
        assert!(runner.state().objects[&source]
            .static_definitions
            .is_empty());
        if mode == "expired" {
            // The producer installed a real live effect. Retain its lapsed
            // entry to exercise liveness before the ordinary pruning sweep.
            runner.state_mut().objects.get_mut(&source).unwrap().zone = Zone::Graveyard;
        }
        let expected = match mode {
            "dynamic" => 2,
            "fixed" => 3,
            _ => 4,
        };
        if mode != "dynamic" {
            runner.state_mut().players[0].mana_pool = (0..expected)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect();
        }
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(ManaCost::generic(if mode == "dynamic" {
                1
            } else {
                expected
            })),
            "{mode}: the public grant supplies the pre-announcement preview"
        );
        begin_manual(&mut runner, spell);
        assert!(
            !engine::game::functioning_abilities::game_functioning_statics(runner.state())
                .any(|(_, definition)| matches!(definition.mode, StaticMode::ModifyCost { .. }))
        );
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            ManaCost::generic(expected)
        );
        if mode == "dynamic" {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert_eq!(
                count(runner.state(), spell, artifact_in(Zone::Battlefield)),
                0
            );
            assert!(!runner
                .state()
                .transient_continuous_effects
                .iter()
                .any(|effect| effect.id == effect_id));
            assert_eq!(
                runner.state().pending_cast.as_ref().unwrap().cost,
                ManaCost::generic(2)
            );
        }
        runner.act(GameAction::PassPriority).unwrap();
        assert!(runner.state().pending_cast.is_none());
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    }
}

#[test]
fn pending_population_functioning_modifier_sources() {
    use engine::types::ability::StaticDefinition;
    for source_kind in ["battlefield", "command", "object grant"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_artifact_to_hand_from_oracle(P0, "Synthetic functioning-source artifact", "")
            .with_mana_cost(ManaCost::generic(4))
            .id();
        let mana_source = scenario
            .add_artifact_from_oracle(
                P0,
                "Functioning-source mana artifact",
                "Sacrifice this artifact: Add {C}{C}.",
            )
            .id();
        scenario.add_artifact_to_hand_from_oracle(P0, "Functioning-source hand member", "");
        let mut definition = population_modifier(true);
        if source_kind == "command" {
            definition.active_zones = vec![Zone::Command];
        }
        let host = if source_kind == "object grant" {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic modifier host", "")
                .id()
        } else {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic modifier host", "")
                .with_static_definition(definition.clone())
                .id()
        };
        if source_kind == "object grant" {
            scenario
                .add_enchantment_from_oracle(P0, "Synthetic object-bound granter", "")
                .with_static_definition(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SpecificObject { id: host })
                        .modifications(vec![ContinuousModification::GrantStaticAbility {
                            definition: Box::new(definition),
                        }]),
                );
        }
        let mut runner = scenario.build();
        if source_kind == "command" {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                host,
                Zone::Command,
                &mut Vec::new(),
            );
        }
        engine::game::layers::flush_layers(runner.state_mut());
        assert_eq!(
            engine::game::functioning_abilities::game_functioning_statics(runner.state())
                .filter(|(object, definition)| {
                    object.id == host && matches!(definition.mode, StaticMode::ModifyCost { .. })
                })
                .count(),
            1,
            "{source_kind}: the intended host must supply the modifier"
        );
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P0, spell),
            Some(ManaCost::generic(1)),
            "{source_kind}: preview counts the unannounced hand artifact"
        );
        begin_manual(&mut runner, spell);
        assert!(runner.state().objects[&spell].static_definitions.is_empty());
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            ManaCost::generic(2),
            "{source_kind}"
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: mana_source,
                ability_index: 0,
            })
            .unwrap();
        assert_eq!(
            runner.state().pending_cast.as_ref().unwrap().cost,
            ManaCost::generic(2)
        );
        runner.act(GameAction::PassPriority).unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    }
}

#[test]
fn pending_population_modifier_order() {
    use engine::types::ability::StaticDefinition;
    use engine::types::statics::CostReductionReach;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 5);
    let spell = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic floor artifact", REPEATED)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.add_artifact_from_oracle(P0, "Floor battlefield member", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Floor hand member", "");
    for (mode, amount) in [(CostModifyMode::Raise, 2), (CostModifyMode::Minimum, 5)] {
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic cost modifier", "")
            .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
                mode,
                amount: ManaCost::generic(amount),
                spell_filter: None,
                dynamic_count: None,
                reach: CostReductionReach::SpillsToGeneric,
            }));
    }
    let mut runner = scenario.build();
    begin_manual(&mut runner, spell);
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(5)
    );
    runner.act(GameAction::PassPriority).unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
}

fn alternative_permission(
    granted_to: engine::types::player::PlayerId,
    base: u32,
    rider: u32,
) -> engine::types::ability::CastingPermission {
    use engine::types::ability::{CastCostModifier, CastingPermission, ExileGrantCostProvenance};
    CastingPermission::ExileWithAltCost {
        cost: ManaCost::generic(base),
        cost_provenance: ExileGrantCostProvenance::Alternative,
        cast_transformed: false,
        constraint: None,
        granted_to: Some(granted_to),
        resolution_cleanup: None,
        duration: None,
        source_id: None,
        graveyard_replacement: None,
        enters_with_counter: None,
        enters_with_modifications: Vec::new(),
        mana_spend_permission: None,
        cast_cost_modifier: Some(CastCostModifier::reduce(ManaCost::generic(rider))),
    }
}

fn assert_pending_reader_authorities(
    runner: &GameRunner,
    spell: ObjectId,
    origin: Zone,
    record: &engine::types::game_state::ZoneChangeRecord,
) {
    use engine::game::filter::{
        matches_target_filter, matches_target_filter_on_zone_change_record, FilterContext,
    };
    let ctx = FilterContext::from_source_with_controller(spell, P0);
    assert_eq!(runner.state().objects[&spell].owner, P1);
    assert_eq!(
        runner.state().objects[&spell].controller,
        P1,
        "retained origin still has its original facts"
    );
    let exact = |filter| TargetFilter::And {
        filters: vec![TargetFilter::SpecificObject { id: spell }, filter],
    };
    assert_eq!(
        count(runner.state(), spell, exact(artifact_in(Zone::Stack))),
        1
    );
    assert_eq!(count(runner.state(), spell, exact(artifact_in(origin))), 0);
    assert!(matches_target_filter(
        runner.state(),
        spell,
        &artifact_in(origin),
        &ctx
    ));
    assert!(!matches_target_filter(
        runner.state(),
        spell,
        &artifact_in(Zone::Stack),
        &ctx
    ));
    assert!(matches_target_filter_on_zone_change_record(
        runner.state(),
        record,
        &artifact_in(origin),
        &ctx
    ));
    assert!(!matches_target_filter_on_zone_change_record(
        runner.state(),
        record,
        &artifact_in(Zone::Stack),
        &ctx
    ));
    let stack_scope = |controller| {
        TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .controller(controller)
                .properties(vec![FilterProp::InZone { zone: Zone::Stack }]),
        )
    };
    assert_eq!(
        count(
            runner.state(),
            spell,
            exact(stack_scope(ControllerRef::You))
        ),
        1
    );
    assert_eq!(
        count(
            runner.state(),
            spell,
            exact(stack_scope(ControllerRef::Opponent))
        ),
        0
    );
    let owner = TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Owned {
        controller: ControllerRef::Opponent,
    }]));
    assert_eq!(
        count(
            runner.state(),
            spell,
            exact(TargetFilter::And {
                filters: vec![
                    TargetFilter::Or {
                        filters: vec![stack_scope(ControllerRef::You), artifact_in(origin)]
                    },
                    TargetFilter::Not {
                        filter: Box::new(stack_scope(ControllerRef::Opponent))
                    },
                    owner,
                ]
            })
        ),
        1
    );
}

#[test]
fn pending_population_selected_permission_cost() {
    use engine::types::ability::{CastingPermission, TargetRef};
    use engine::types::game_state::CastingPermissionIndex;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 3);
    let text = format!("{REPEATED}\nTarget player gains 1 life.");
    let spell = scenario
        .add_creature_to_exile(P1, "Synthetic foreign permission artifact", 2, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(9))
        .from_oracle_text(&text)
        .with_keyword(Keyword::Flash)
        .id();
    scenario.add_artifact_from_oracle(P0, "Permission battlefield member", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Permission hand member", "");
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic unrelated spell", true, "You gain 2 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let ability_source = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic unrelated activation",
            1,
            1,
            "{0}: You gain 1 life.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(unrelated).commit();
    runner
        .state_mut()
        .stack
        .iter_mut()
        .find(|entry| entry.id == unrelated)
        .unwrap()
        .controller = P1;
    runner
        .act(GameAction::ActivateAbility {
            source_id: ability_source,
            ability_index: 0,
        })
        .unwrap();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions = vec![
        alternative_permission(P1, 1, 0),
        alternative_permission(P0, 6, 1),
    ];
    let record = runner.state().objects[&spell].snapshot_for_zone_change(
        spell,
        Some(Zone::Exile),
        Zone::Stack,
    );
    let before = runner.state().stack.len();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("selected permission must pause for the printed target");
    };
    assert_eq!(
        pending_cast.casting_permission_index,
        Some(CastingPermissionIndex(1))
    );
    assert_eq!(pending_cast.base_cost, Some(ManaCost::generic(6)));
    let CastingPermission::ExileWithAltCost { granted_to, .. } = &mut runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions[0]
    else {
        unreachable!()
    };
    *granted_to = Some(P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .unwrap();
    assert_eq!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .casting_permission_index,
        Some(CastingPermissionIndex(1)),
        "a newly eligible sibling cannot replace the selected authority"
    );
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().base_cost,
        Some(ManaCost::generic(6))
    );
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(3)
    );
    assert_pending_reader_authorities(&runner, spell, Zone::Exile, &record);
    runner.act(GameAction::CancelCast).unwrap();
    assert_eq!(runner.state().stack.len(), before);
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.id == unrelated && entry.controller == P1));
    assert_eq!(
        count(
            runner.state(),
            spell,
            TargetFilter::And {
                filters: vec![
                    artifact_in(Zone::Stack),
                    TargetFilter::SpecificObject { id: spell }
                ]
            }
        ),
        0
    );
    assert_eq!(runner.state().objects[&spell].owner, P1);
    // Restore the original eligibility and cast anew through the fluent driver.
    let CastingPermission::ExileWithAltCost { granted_to, .. } = &mut runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions[0]
    else {
        unreachable!()
    };
    *granted_to = Some(P1);
    let outcome = runner.cast(spell).target_player(P0).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    outcome.assert_zone(&[spell], Zone::Battlefield);
}

#[test]
fn pending_foreign_spell_controller() {
    use engine::types::ability::{CardPlayMode, StaticDefinition};
    use engine::types::statics::{CastFrequency, GraveyardPermissionPool};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 2);
    let spell = scenario
        .add_creature_to_graveyard(P1, "Synthetic foreign graveyard artifact", 2, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text(REPEATED)
        .id();
    scenario.add_artifact_from_oracle(P0, "Graveyard battlefield member", "");
    scenario.add_artifact_to_hand_from_oracle(P0, "Graveyard hand member", "");
    scenario
        .add_enchantment_from_oracle(P0, "Synthetic any-graveyard permission", "")
        .with_static_definition(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
                required_cast_keyword: None,
                pool: GraveyardPermissionPool::AnyGraveyard,
            })
            .affected(TargetFilter::Typed(TypedFilter::creature())),
        );
    let mut runner = scenario.build();
    let record = runner.state().objects[&spell].snapshot_for_zone_change(
        spell,
        Some(Zone::Graveyard),
        Zone::Stack,
    );
    begin_manual(&mut runner, spell);
    assert_eq!(
        runner.state().pending_cast.as_ref().unwrap().cost,
        ManaCost::generic(2)
    );
    assert_pending_reader_authorities(&runner, spell, Zone::Graveyard, &record);
    runner.act(GameAction::CancelCast).unwrap();
    assert_eq!(runner.state().objects[&spell].owner, P1);
    let outcome = runner.cast(spell).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    outcome.assert_zone(&[spell], Zone::Battlefield);
}

#[test]
fn pending_population_reader_authorities() {
    use engine::game::filter::{
        matches_target_filter, matches_target_filter_on_zone_change_record, FilterContext,
    };
    use engine::types::ability::Comparator;
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    use engine::types::mana::ManaColor;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    mana(&mut scenario, 4);
    let spell = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic characteristic carrier", "")
        .with_keyword(Keyword::Flash)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let other = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic competing spell", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(other).commit();
    let other_index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == other)
        .unwrap();
    let unrelated_ability = runner.state().stack[other_index].ability().unwrap().clone();
    if let StackEntryKind::Spell {
        casting_variant, ..
    } = &mut runner.state_mut().stack[other_index].kind
    {
        *casting_variant = CastingVariant::Fuse;
    }
    let record = runner.state().objects[&spell].snapshot_for_zone_change(
        spell,
        Some(Zone::Hand),
        Zone::Stack,
    );
    begin_manual(&mut runner, spell);
    let index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == spell)
        .unwrap();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .back_face = Some(BackFaceData {
        name: "Synthetic other Split half".into(),
        layout_kind: Some(LayoutKind::Split),
        mana_cost: ManaCost::generic(3),
        color: vec![ManaColor::Red],
        ..Default::default()
    });
    let characteristic_filter = TargetFilter::And {
        filters: vec![
            artifact_in(Zone::Stack),
            TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 5 },
                },
                FilterProp::HasColor {
                    color: ManaColor::Red,
                },
            ])),
        ],
    };
    // The unrelated Fuse entry cannot supply this spell's characteristics.
    assert_eq!(
        count(runner.state(), spell, characteristic_filter.clone()),
        0
    );
    if let StackEntryKind::Spell {
        casting_variant, ..
    } = &mut runner.state_mut().stack[index].kind
    {
        *casting_variant = CastingVariant::Fuse;
    }
    assert_eq!(count(runner.state(), spell, characteristic_filter), 1);
    assert!(
        !runner.state().objects[&spell].fused_split_spell,
        "projection never persists the hint early"
    );
    let ctx = FilterContext::from_source_with_controller(spell, P0);
    assert!(matches_target_filter(
        runner.state(),
        spell,
        &artifact_in(Zone::Hand),
        &ctx
    ));
    assert!(matches_target_filter_on_zone_change_record(
        runner.state(),
        &record,
        &artifact_in(Zone::Hand),
        &ctx
    ));
    assert!(!matches_target_filter_on_zone_change_record(
        runner.state(),
        &record,
        &artifact_in(Zone::Stack),
        &ctx
    ));
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 1);
    let announcement = runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .spell_announcement
        .take();
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .spell_announcement = announcement;
    runner.state_mut().stack[index].kind = StackEntryKind::ActivatedAbility {
        source_id: spell,
        ability: Box::new(unrelated_ability),
    };
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Stack)), 0);
    assert_eq!(count(runner.state(), spell, artifact_in(Zone::Hand)), 1);
}
