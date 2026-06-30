//! Regression test for GitHub issue #1516 — the Craft keyword (CR 702.167).
//!
//! Tithing Blade // Consuming Sepulcher carries `Keyword::Craft`, but before
//! this fix the keyword synthesized no activated ability, so the card never
//! offered to craft. The fix:
//!   1. enriches `Keyword::Craft` to carry the materials class + count,
//!   2. synthesizes a sorcery-speed activated ability whose cost is
//!      `Composite[Mana, Exile{SelfRef}, ExileMaterials]` and whose effect
//!      returns the card from exile transformed (CR 712.14a), and
//!   3. wires an interactive `WaitingFor::PayCost { kind: ExileMaterials }`
//!      detour so the player chooses which materials to exile across the
//!      battlefield/graveyard union (CR 702.167b).
//!
//! Two layers of coverage:
//!   * An offline scenario (no card DB required) builds a craft permanent with a
//!     synthesized craft ability and drives activation → materials detour →
//!     resolution, plus the negative paths (instant speed, zero materials).
//!   * A DB-gated test (skipped when `card-data.json` is absent) loads the real
//!     Tithing Blade and asserts the full transform-on-return behavior.
//!
//! CR 702.167a/b: Craft is a sorcery-speed activated ability that exiles the
//! source plus materials and returns the card transformed.
//! CR 712.14 / 712.14a: a DFC put onto the battlefield transformed enters back
//! face up.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, CostObjectCount, Effect,
    FilterProp, PlayerFilter, QuantityExpr, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 702.167b: The dual-zone materials filter for "craft with creature" —
/// a creature permanent you control OR a creature card in your graveyard.
fn craft_with_creature_materials() -> TargetFilter {
    let battlefield = TargetFilter::Typed(
        TypedFilter::permanent()
            .with_type(TypeFilter::Creature)
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }]),
    );
    let graveyard = TargetFilter::Typed(
        TypedFilter::card()
            .with_type(TypeFilter::Creature)
            .properties(vec![
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
            ]),
    );
    TargetFilter::Or {
        filters: vec![battlefield, graveyard],
    }
}

/// The synthesized craft ability for "craft with creature {1}" (cheap cost so
/// the test funds it trivially).
fn craft_ability(cost: ManaCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::ChangeZone {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: true,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost },
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Battlefield),
                filter: Some(TargetFilter::SelfRef),
            },
            AbilityCost::ExileMaterials {
                materials: craft_with_creature_materials(),
                count: CostObjectCount::exactly(1),
            },
        ],
    })
    .sorcery_speed()
}

fn fund(runner: &mut engine::game::scenario::GameRunner, count: u32, mana_type: ManaType) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }
}

/// CR 702.167a/b: Activating a craft ability at sorcery speed surfaces the
/// materials detour with the eligible creature in `choices` and the source
/// excluded; selecting it exiles both source and material.
#[test]
fn craft_activation_offers_materials_detour_and_exiles_source_plus_material() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The craft artifact (source). One generic mana cost so funding is easy.
    let blade_id = scenario
        .add_creature(P0, "Test Craft Blade", 0, 0)
        .as_artifact()
        .with_keyword(Keyword::Craft {
            cost: ManaCost::Cost {
                shards: vec![],
                generic: 1,
            },
            materials: craft_with_creature_materials(),
            count: CostObjectCount::exactly(1),
        })
        .with_ability_definition(craft_ability(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }))
        .id();

    // An eligible creature material under P0.
    let creature_id = scenario.add_creature(P0, "Eligible Bear", 2, 2).id();
    scenario.with_life(P0, 20);

    let mut runner = scenario.build();
    fund(&mut runner, 1, ManaType::Colorless);

    // P0 is active in its precombat main — sorcery timing is legal.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: blade_id,
            ability_index: 0,
        })
        .expect("activating craft at sorcery speed is allowed");

    match result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileMaterials { .. },
            ref choices,
            ..
        } => {
            assert!(
                choices.contains(&creature_id),
                "the eligible creature must be offered as a craft material"
            );
            assert!(
                !choices.contains(&blade_id),
                "CR 702.167a: the source's self-exile is a separate cost — it \
                 must NOT appear among the material choices"
            );
        }
        other => panic!("expected ExileMaterials PayCost detour, got {other:?}"),
    }

    // Select the creature as the material.
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature_id],
        })
        .expect("selecting the material is accepted");

    // Drive the activation to resolution.
    runner.advance_until_stack_empty();

    // Both the source and the material end up exiled (the source returns on
    // resolution; without a hydrated DFC back face it returns to the
    // battlefield, but the material stays exiled).
    assert_eq!(
        runner.state().objects[&creature_id].zone,
        Zone::Exile,
        "the chosen creature material must be exiled (CR 702.167a)"
    );
}

/// CR 702.167a: "Activate only as a sorcery." Activating during an opponent's
/// turn / with the stack non-empty is illegal.
#[test]
fn craft_cannot_be_activated_at_instant_speed() {
    let mut scenario = GameScenario::new();
    // Opponent's turn → not the craft controller's main phase, so sorcery
    // timing is violated.
    scenario.at_phase(Phase::PreCombatMain);

    let blade_id = scenario
        .add_creature(P0, "Test Craft Blade", 0, 0)
        .as_artifact()
        .with_ability_definition(craft_ability(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }))
        .id();
    let _creature_id = scenario.add_creature(P0, "Eligible Bear", 2, 2).id();
    scenario.with_life(P0, 20);

    let mut runner = scenario.build();
    fund(&mut runner, 1, ManaType::Colorless);
    // Force the active player to the opponent so P0's sorcery-speed activation
    // is illegal (not P0's main phase with an empty stack and priority).
    runner.state_mut().active_player = engine::game::scenario::P1;

    let err = runner.act(GameAction::ActivateAbility {
        source_id: blade_id,
        ability_index: 0,
    });
    assert!(
        err.is_err(),
        "craft must be rejected when activated outside the controller's main \
         phase (CR 702.167a: activate only as a sorcery)"
    );
}

/// CR 702.167a/b + CR 601.2b: With zero eligible materials, activation is
/// rejected — there is nothing to exile to pay the materials cost.
#[test]
fn craft_rejected_with_zero_eligible_materials() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blade_id = scenario
        .add_creature(P0, "Test Craft Blade", 0, 0)
        .as_artifact()
        .with_ability_definition(craft_ability(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }))
        .id();
    // No other creatures under P0 — the blade itself is excluded as a material.
    scenario.with_life(P0, 20);

    let mut runner = scenario.build();
    fund(&mut runner, 1, ManaType::Colorless);

    let err = runner.act(GameAction::ActivateAbility {
        source_id: blade_id,
        ability_index: 0,
    });
    assert!(
        err.is_err(),
        "craft must be rejected with no eligible materials (CR 601.2b)"
    );
}

/// CR 702.167a/b + CR 712.14a: The real Tithing Blade crafts into Consuming
/// Sepulcher (transformed). Skipped when the card DB is unavailable.
#[test]
fn tithing_blade_crafts_into_transformed_sepulcher() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blade = scenario.add_real_card(P0, "Tithing Blade", Zone::Battlefield, db);
    let material = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    scenario.with_life(P0, 20);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Fund the craft cost {4}{B}.
    fund(&mut runner, 4, ManaType::Colorless);
    {
        let pool = &mut runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool;
        pool.add(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
    }

    // Find the synthesized craft ability index on the blade.
    let craft_index = runner.state().objects[&blade]
        .abilities
        .iter()
        .position(|a| {
            matches!(a.kind, AbilityKind::Activated)
                && a.cost
                    .as_ref()
                    .map(cost_has_exile_materials)
                    .unwrap_or(false)
        })
        .expect("Tithing Blade must synthesize a craft activated ability");

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: blade,
            ability_index: craft_index,
        })
        .expect("activate craft");

    match result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileMaterials { .. },
            ref choices,
            ..
        } => {
            assert!(choices.contains(&material), "creature must be offerable");
            assert!(!choices.contains(&blade), "source must be excluded");
        }
        other => panic!("expected ExileMaterials detour, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![material],
        })
        .expect("select material");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&material].zone,
        Zone::Exile,
        "the material must be exiled"
    );
    let blade_obj = &runner.state().objects[&blade];
    assert_eq!(
        blade_obj.zone,
        Zone::Battlefield,
        "the blade must return to the battlefield"
    );
    assert!(
        blade_obj.transformed,
        "the blade must return transformed into Consuming Sepulcher (CR 712.14a)"
    );
}

/// A back face that distinguishes the crafted-into permanent ("Consuming
/// Sepulcher") from the front-face blade. Built offline so the transform-on-
/// return path is exercised without the real card DB (mirrors the back-face
/// idiom in `game/transform.rs` tests and `integration_bending.rs`).
fn consuming_sepulcher_back_face() -> BackFaceData {
    BackFaceData {
        name: "Consuming Sepulcher".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        // CR 712.14a: the back face is the permanent that exists after return.
        // Consuming Sepulcher is an Enchantment — distinct core type from the
        // front-face Artifact so the test can assert identity flipped.
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Enchantment],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Black],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

/// CR 702.167a + CR 712.14a: The discriminating end-to-end regression for
/// #1516 — entirely offline. Crafting the blade exiles it plus the chosen
/// material, then returns the blade to the battlefield **transformed** into its
/// back face (Consuming Sepulcher); the material stays in exile.
///
/// This is the test that is red on `main`: there, `Keyword::Craft` is a tuple
/// variant with no `synthesize_craft`, no `AbilityCost::ExileMaterials`, and no
/// `PayCostKind::ExileMaterials` — so this file does not even compile, let alone
/// drive a craft to a transformed return. Here it compiles and the blade flips.
///
/// CR 603.6a: the returning object is a *new* permanent that is the back face,
/// so the front face's enters-the-battlefield ability is not the entering
/// object's ability and must not fire — the opponent's creature is untouched.
#[test]
fn craft_returns_blade_transformed_and_front_etb_does_not_refire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The craft artifact (source). One generic mana cost so funding is easy.
    // CR 603.6a: give the FRONT face an ETB that makes each opponent sacrifice a
    // creature (the real Tithing Blade ETB). If this fired on the transform-
    // return it would destroy P1's creature — asserting P1's creature survives
    // proves the front-face ETB does not re-fire (the returnee is the back face).
    let front_etb = TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        // Self-only ETB: fires for the blade itself entering.
        .valid_card(TargetFilter::SelfRef)
        .description("When this artifact enters, each opponent sacrifices a creature.".to_string())
        .execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Sacrifice {
                    target: TargetFilter::Typed(
                        TypedFilter::permanent().with_type(TypeFilter::Creature),
                    ),
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
            )
            // CR 701.21a: "each opponent sacrifices" rebinds the chooser to each
            // opponent of the controller.
            .player_scope(PlayerFilter::Opponent),
        );

    let blade_id = scenario
        .add_creature(P0, "Test Craft Blade", 0, 0)
        .as_artifact()
        .with_keyword(Keyword::Craft {
            cost: ManaCost::Cost {
                shards: vec![],
                generic: 1,
            },
            materials: craft_with_creature_materials(),
            count: CostObjectCount::exactly(1),
        })
        .with_ability_definition(craft_ability(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        }))
        .with_trigger_definition(front_etb)
        .id();

    // An eligible creature material under P0.
    let creature_id = scenario.add_creature(P0, "Eligible Bear", 2, 2).id();
    // An opponent creature — the front-face ETB would sacrifice this if it fired.
    let opp_creature_id = scenario.add_creature(P1, "Opponent Ox", 2, 2).id();
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    let mut runner = scenario.build();

    // Give the blade a back face so the craft return actually transforms it.
    runner
        .state_mut()
        .objects
        .get_mut(&blade_id)
        .unwrap()
        .back_face = Some(consuming_sepulcher_back_face());

    fund(&mut runner, 1, ManaType::Colorless);

    // Activate craft at sorcery speed; expect the materials detour with the
    // bear offered and the source excluded (CR 702.167a/b).
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: blade_id,
            ability_index: 0,
        })
        .expect("activating craft at sorcery speed is allowed");

    match result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileMaterials { .. },
            ref choices,
            ..
        } => {
            assert!(
                choices.contains(&creature_id),
                "the eligible creature must be offered as a craft material"
            );
            assert!(
                !choices.contains(&blade_id),
                "CR 702.167a: the source's self-exile is a separate cost — it \
                 must NOT appear among the material choices"
            );
        }
        other => panic!("expected ExileMaterials PayCost detour, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![creature_id],
        })
        .expect("selecting the material is accepted");

    runner.advance_until_stack_empty();

    // CR 702.167a: the chosen material stays exiled.
    assert_eq!(
        runner.state().objects[&creature_id].zone,
        Zone::Exile,
        "the chosen creature material must remain exiled (CR 702.167a)"
    );

    // CR 712.14a: the blade returns to the battlefield transformed, now the
    // back face (Consuming Sepulcher).
    let blade_obj = &runner.state().objects[&blade_id];
    assert_eq!(
        blade_obj.zone,
        Zone::Battlefield,
        "the blade must return to the battlefield (CR 702.167a)"
    );
    assert!(
        blade_obj.transformed,
        "the blade must return transformed into Consuming Sepulcher (CR 712.14a)"
    );
    assert_eq!(
        blade_obj.name, "Consuming Sepulcher",
        "the returned permanent's identity is the back face (CR 712.14a)"
    );
    assert!(
        blade_obj
            .card_types
            .core_types
            .contains(&CoreType::Enchantment),
        "the returned permanent carries the back face's card types (CR 712.14a)"
    );

    // CR 603.6a: the returnee is the back face, a new object whose abilities are
    // the back face's — the front-face ETB is not its ability and must not fire,
    // so the opponent's creature is untouched.
    assert_eq!(
        runner.state().objects[&opp_creature_id].zone,
        Zone::Battlefield,
        "the front-face ETB must not re-fire on the transform-return — the \
         opponent's creature must survive (CR 603.6a / CR 712.14a)"
    );

    // No front-face-ETB sacrifice ability should be sitting on the stack either.
    assert!(
        runner.state().stack.is_empty(),
        "no spurious front-face ETB ability should be on the stack after the \
         transform-return (CR 603.6a)"
    );
}

fn cost_has_exile_materials(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::ExileMaterials { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(cost_has_exile_materials),
        _ => false,
    }
}
