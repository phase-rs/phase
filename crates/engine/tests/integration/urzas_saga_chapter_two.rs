//! Integration test: Urza's Saga chapter II.
//!
//! Chapter II grants the Saga a `{2}, {T}: Create a 0/0 colorless Construct
//! artifact creature token with "This token gets +1/+1 for each artifact you
//! control."` activated ability. The token enters as 0/0; only the +1/+1
//! boost saves it from SBAs (CR 704.5f). This test exercises the full
//! pipeline: parser populates `static_abilities` on the Token effect → token
//! resolver mirrors them onto `base_static_definitions` → layer 7c reads the
//! `AddDynamicPower` modification and computes effective power from the
//! artifact count → SBA leaves the token alive.

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
    PtValue, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Fill a player's mana pool directly.
fn add_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: PlayerId,
    color: ManaType,
    count: usize,
) {
    let state = runner.state_mut();
    let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        p.mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
    }
}

/// Build the granted activated ability that chapter II would install on the
/// Saga: `{2}, {T}: Create a 0/0 colorless Construct artifact creature token
/// with "This token gets +1/+1 for each artifact you control."`
fn chapter_two_granted_ability() -> AbilityDefinition {
    let artifact_filter =
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You));

    let boost = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: artifact_filter.clone(),
                    },
                },
            },
            ContinuousModification::AddDynamicToughness {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: artifact_filter,
                    },
                },
            },
        ]);

    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Token {
            name: "Construct".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec![
                "Artifact".to_string(),
                "Creature".to_string(),
                "Construct".to_string(),
            ],
            colors: vec![],
            keywords: vec![] as Vec<Keyword>,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![boost],
            enter_with_counters: vec![],
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            },
            AbilityCost::Tap,
        ],
    })
}

#[test]
fn chapter_two_construct_survives_sba_via_self_count_boost() {
    // Set up Urza's Saga as a battlefield permanent with the chapter II
    // ability already granted (skipping the "increment lore counter →
    // CounterAdded trigger → grant" flow that's exercised elsewhere).
    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let saga_id = {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = zones::create_object(
            state,
            card_id,
            P0,
            "Urza's Saga".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.card_types.subtypes.push("Urza's".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.summoning_sick = false;
        std::sync::Arc::make_mut(&mut obj.abilities).push(chapter_two_granted_ability());
        std::sync::Arc::make_mut(&mut obj.base_abilities).push(chapter_two_granted_ability());
        state.layers_dirty = true;
        id
    };

    add_mana(&mut runner, P0, ManaType::Colorless, 2);

    // Activate the granted {2}, {T} ability.
    let pre_count = runner.state().objects.len();
    let result = runner.act(GameAction::ActivateAbility {
        source_id: saga_id,
        ability_index: 0,
    });
    assert!(
        result.is_ok(),
        "chapter II ability activation must succeed: {:?}",
        result.err()
    );

    // Resolve the ability on the stack — this should create the Construct.
    runner.resolve_top();

    // Find the newly-created Construct token.
    let state = runner.state();
    assert!(
        state.objects.len() > pre_count,
        "a new token object must exist after resolving chapter II's activated ability \
         (pre={pre_count}, post={})",
        state.objects.len()
    );

    let construct = state
        .objects
        .values()
        .find(|o| o.is_token && o.name == "Construct")
        .expect("Construct token must be on the battlefield after resolution");

    assert_eq!(
        construct.zone,
        Zone::Battlefield,
        "Construct must enter the battlefield, not be sent straight to the graveyard"
    );

    // The Construct counts itself as the only artifact P0 controls, so its
    // effective P/T after layer 7c is 1/1 — enough to survive SBAs.
    assert_eq!(
        construct.power,
        Some(1),
        "Construct must have +1 power from the self-counted artifact static (it itself is an artifact)"
    );
    assert_eq!(
        construct.toughness,
        Some(1),
        "Construct must have +1 toughness — without this it dies to CR 704.5f as a 0/0"
    );
}
