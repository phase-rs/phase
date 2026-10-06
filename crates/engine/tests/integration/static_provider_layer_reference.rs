//! A live provider-membership reference for a layer-6 ability grant.
//! March of the Machines changes Sol Ring's type in layer 4; Marvin's printed
//! static ability must then see Sol Ring as a creature in layer 6.

use std::sync::Arc;

use engine::game::casting::{activated_ability_definitions, can_activate_ability_now};
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, Effect, FilterProp,
    QuantityExpr, StaticCondition, StaticDefinition, TargetFilter, TriggerDefinition,
    TriggerDefinitionOccurrenceRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::LayersDirty;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const MARVIN: &str = "Marvin has all activated abilities of creatures you control that don't have the same name as this creature.";
const MARCH: &str = "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)";
const SOL_RING: &str = "{T}: Add {C}{C}.";
const PRESENCE_OF_GOND: &str = "Enchant creature\nEnchanted creature has \"{T}: Create a 1/1 green Elf Warrior creature token.\"";

#[test]
fn march_animation_updates_marvins_live_activated_ability_providers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature(P0, "Marvin, Murderous Mimic", 2, 2)
        .as_artifact_creature()
        .as_legendary()
        .with_subtypes(vec!["Toy"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .from_oracle_text(MARVIN)
        .id();
    let ring = scenario
        .add_artifact_from_oracle(P0, "Sol Ring", SOL_RING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    let march = scenario
        .add_spell_to_hand(P0, "March of the Machines", false)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .from_oracle_text(MARCH)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, marvin, false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();

    // Positive parser guards: the host has its printed grant and the donor has
    // precisely the printed tap-for-two ability before any layer interaction.
    assert!(runner.state().objects[&marvin]
        .base_static_definitions
        .iter()
        .flat_map(|definition| &definition.modifications)
        .any(|modification| matches!(
            modification,
            ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        )));
    let donor_abilities = activated_ability_definitions(runner.state(), ring);
    assert_eq!(donor_abilities.len(), 1, "Sol Ring must parse one ability");
    let donor = &donor_abilities[0].1;
    assert_eq!(donor.kind, AbilityKind::Activated);
    assert_eq!(donor.cost, Some(AbilityCost::Tap));
    assert!(matches!(donor.effect.as_ref(), Effect::Mana { .. }));

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(!runner.state().objects[&ring]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    // CR 611.3a + CR 613.1f: a noncreature Sol Ring is not yet a provider.
    assert!(activated_ability_definitions(runner.state(), marvin).is_empty());

    // CR 601.2 + CR 611.3b: resolve the real enchantment from hand, making its
    // static type change active on the battlefield.
    runner
        .cast(march)
        .resolve()
        .assert_zone(&[march], Zone::Battlefield);
    assert!(
        runner.state().objects[&ring]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "March must animate Sol Ring before Marvin's grant is checked"
    );

    // CR 613.1d + CR 613.1f: after the layer-4 type change, Marvin gains the
    // donor's exact activated cost and effect in layer 6.
    let granted = activated_ability_definitions(runner.state(), marvin);
    assert_eq!(granted.len(), 1, "Marvin must gain Sol Ring's mana ability");
    let (ability_index, ability) = &granted[0];
    assert_eq!(ability.kind, AbilityKind::Activated);
    assert_eq!(ability.cost, donor.cost);
    assert_eq!(ability.effect, donor.effect);
    assert!(can_activate_ability_now(
        runner.state(),
        P0,
        marvin,
        *ability_index
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        0
    );

    // CR 602.2 + CR 605.3b: the donated mana ability belongs to Marvin, so
    // Marvin taps and produces two colorless mana; the donor remains untapped.
    runner
        .act(GameAction::ActivateAbility {
            source_id: marvin,
            ability_index: *ability_index,
        })
        .expect("Marvin must activate Sol Ring's donated mana ability");
    assert!(runner.state().objects[&marvin].tapped);
    assert!(!runner.state().objects[&ring].tapped);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        2
    );

    // CR 611.3a-b: source removal makes Sol Ring a noncreature again, so it
    // leaves Marvin's live provider set on the next normal layer flush.
    move_to_zone(runner.state_mut(), march, Zone::Graveyard, &mut Vec::new());
    flush_layers(runner.state_mut());
    assert!(!runner.state().objects[&ring]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(activated_ability_definitions(runner.state(), marvin).is_empty());
}

#[test]
fn older_marvin_copies_an_ability_granted_to_a_donor_by_later_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .as_artifact_creature()
        .as_legendary()
        .with_subtypes(vec!["Toy"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();
    let donor = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let gond = scenario
        .add_spell_to_hand(P0, "Presence of Gond", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Enchant"], PRESENCE_OF_GOND)
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Colorless, ManaType::Green]
            .into_iter()
            .map(|color| ManaUnit::new(color, marvin, false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    assert!(runner.state().objects[&marvin]
        .base_static_definitions
        .iter()
        .flat_map(|definition| &definition.modifications)
        .any(|modification| matches!(
            modification,
            ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        )));
    assert!(activated_ability_definitions(runner.state(), donor).is_empty());

    // CR 303.4a + CR 613.1f: resolving the Aura onto the donor grants its
    // printed token-creating activated ability in the same layer as Marvin's.
    runner
        .cast(gond)
        .target_object(donor)
        .resolve()
        .assert_zone(&[gond], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&gond].attached_to,
        Some(donor.into())
    );
    assert!(runner.state().objects[&marvin].timestamp < runner.state().objects[&gond].timestamp);

    let donor_abilities = activated_ability_definitions(runner.state(), donor);
    assert_eq!(
        donor_abilities.len(),
        1,
        "Gond must grant the donor one ability"
    );
    let donor_ability = &donor_abilities[0].1;
    assert_eq!(donor_ability.kind, AbilityKind::Activated);
    assert_eq!(donor_ability.cost, Some(AbilityCost::Tap));
    assert!(matches!(
        donor_ability.effect.as_ref(),
        Effect::Token { .. }
    ));

    // CR 613.8a-b + CR 613.1f: the older Marvin grant depends on the later
    // Aura grant because it changes which abilities Marvin copies from donor.
    let granted = activated_ability_definitions(runner.state(), marvin);
    assert_eq!(granted.len(), 1, "Marvin must gain Gond's donated ability");
    let (ability_index, ability) = &granted[0];
    assert_eq!(ability.kind, AbilityKind::Activated);
    assert_eq!(ability.cost, donor_ability.cost);
    assert_eq!(ability.effect, donor_ability.effect);
    assert!(can_activate_ability_now(
        runner.state(),
        P0,
        marvin,
        *ability_index
    ));

    // CR 602.2 + CR 303.4e: Marvin owns the copied activation and pays its tap
    // cost; the enchanted donor does not tap when Marvin creates the Elf.
    runner.activate(marvin, *ability_index).resolve();
    assert!(runner.state().objects[&marvin].tapped);
    assert!(!runner.state().objects[&donor].tapped);
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| {
                object.is_token && object.zone == Zone::Battlefield && object.controller == P0
            })
            .count(),
        1,
        "Marvin's activation must create one Elf token"
    );
    assert!(runner.state().stack.is_empty());

    // CR 611.3a: The donated ability ceases with the attached Aura. The
    // donor remains the same creature, so this isolates the live definition
    // read from provider membership changes.
    move_to_zone(runner.state_mut(), gond, Zone::Graveyard, &mut Vec::new());
    flush_layers(runner.state_mut());
    assert!(activated_ability_definitions(runner.state(), donor).is_empty());
    assert!(activated_ability_definitions(runner.state(), marvin).is_empty());
}

#[test]
fn aura_on_an_opponents_creature_does_not_change_marvins_provider_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .as_artifact_creature()
        .as_legendary()
        .with_subtypes(vec!["Toy"])
        .id();
    let other = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let gond = scenario
        .add_spell_to_hand(P0, "Presence of Gond", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Enchant"], PRESENCE_OF_GOND)
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Colorless, ManaType::Green]
            .into_iter()
            .map(|color| ManaUnit::new(color, marvin, false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(gond).target_object(other).resolve();

    // CR 109.5 + CR 613.8a: Gond reaches and grants the opponent's creature
    // its token ability, but that creature is outside Marvin's "you control"
    // provider set. Its ability cannot create a dependency for this grant.
    assert_eq!(
        activated_ability_definitions(runner.state(), other).len(),
        1
    );
    assert!(activated_ability_definitions(runner.state(), marvin).is_empty());
}

#[test]
fn entering_artifact_animated_in_layer_four_updates_existing_marvin() {
    let mut scenario = GameScenario::new();
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .as_artifact_creature()
        .as_legendary()
        .with_subtypes(vec!["Toy"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", MARCH);
    let ring = scenario
        .add_artifact_from_oracle(P0, "Sol Ring", SOL_RING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    move_to_zone(runner.state_mut(), ring, Zone::Hand, &mut Vec::new());
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(activated_ability_definitions(runner.state(), marvin).is_empty());
    assert_eq!(activated_ability_definitions(runner.state(), ring).len(), 1);

    // CR 611.3a + CR 613.1d: The entrant is a noncreature artifact at the
    // incremental gate, then becomes a creature before Marvin reads providers.
    // Marvin is pre-existing and must be re-derived with the new provider.
    move_to_zone(runner.state_mut(), ring, Zone::Battlefield, &mut Vec::new());
    flush_layers(runner.state_mut());
    assert!(runner.state().objects[&ring]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(
        activated_ability_definitions(runner.state(), marvin).len(),
        1
    );

    move_to_zone(runner.state_mut(), ring, Zone::Graveyard, &mut Vec::new());
    flush_layers(runner.state_mut());
    assert!(activated_ability_definitions(runner.state(), marvin).is_empty());
}

// These fixtures are synthetic typed combinations. They exercise admitted
// layer and trigger building blocks through the ordinary scenario flush/runner;
// none represents invented Oracle text for a printed card.
fn attach_synthetic_static(
    runner: &mut engine::game::scenario::GameRunner,
    source: engine::types::identifiers::ObjectId,
    definition: StaticDefinition,
) {
    let object = runner.state_mut().objects.get_mut(&source).unwrap();
    Arc::make_mut(&mut object.base_static_definitions).push(definition.clone());
    object.static_definitions.push(definition);
}

#[test]
fn coupled_provider_cycles_wait_for_outgoing_dependencies() {
    let mut scenario = GameScenario::new();
    let ids: Vec<_> = ["A", "B", "C", "D"]
        .into_iter()
        .map(|name| {
            let mut ability =
                AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
            ability.description = Some(name.to_string());
            scenario
                .add_creature(P0, name, 1, 1)
                .with_ability_definition(ability)
                .id()
        })
        .collect();
    let [a, b, c, d] = <[_; 4]>::try_from(ids).unwrap();
    let mut runner = scenario.build();
    let sources = [
        TargetFilter::Or {
            filters: vec![
                TargetFilter::SpecificObject { id: b },
                TargetFilter::SpecificObject { id: c },
            ],
        },
        TargetFilter::SpecificObject { id: a },
        TargetFilter::SpecificObject { id: d },
        TargetFilter::SpecificObject { id: c },
    ];
    for (host, source) in [a, b, c, d].into_iter().zip(sources) {
        attach_synthetic_static(
            &mut runner,
            host,
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                    source,
                    cap: None,
                }]),
        );
    }
    assert!(runner.state().objects[&a].timestamp < runner.state().objects[&b].timestamp);
    assert!(runner.state().objects[&b].timestamp < runner.state().objects[&c].timestamp);
    assert!(runner.state().objects[&c].timestamp < runner.state().objects[&d].timestamp);
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    // CR 613.8b-c: A↔B and C↔D are loops, but A also depends on C.
    // D's printed definition must reach C before A copies C and B copies A.
    let descriptions = |id| {
        activated_ability_definitions(runner.state(), id)
            .into_iter()
            .filter_map(|(_, ability)| ability.description)
            .collect::<Vec<_>>()
    };
    assert!(
        descriptions(c).contains(&"D".to_string()),
        "C must receive D"
    );
    assert!(
        descriptions(a).contains(&"D".to_string()),
        "A must receive D through C"
    );
    let b_descriptions = descriptions(b);
    assert!(
        b_descriptions.contains(&"C".to_string()),
        "B must receive C through A"
    );
    assert!(
        b_descriptions.contains(&"D".to_string()),
        "B must receive D through A"
    );
}

#[test]
fn retained_multilayer_trigger_grant_survives_earlier_ability_removal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let remover = scenario.add_creature(P0, "Synthetic Remover", 1, 1).id();
    let host = scenario.add_creature(P0, "Synthetic Host", 1, 1).id();
    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
        ));
    let provider = scenario
        .add_creature(P0, "Synthetic Provider", 1, 1)
        .with_trigger_definition(trigger)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap),
        )
        .id();
    let mut runner = scenario.build();
    attach_synthetic_static(
        &mut runner,
        remover,
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: host })
            .modifications(vec![ContinuousModification::RemoveAllAbilities]),
    );
    attach_synthetic_static(
        &mut runner,
        host,
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Artifact,
                },
                ContinuousModification::GrantAllTriggeredAbilitiesOf {
                    source: TargetFilter::SpecificObject { id: provider },
                },
                ContinuousModification::GrantAllActivatedAbilitiesOf {
                    source: TargetFilter::SpecificObject { id: provider },
                    cap: None,
                },
            ]),
    );
    // This separate layer-6 parent has not begun when removal applies.
    attach_synthetic_static(
        &mut runner,
        host,
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::SpecificObject { id: provider },
            }]),
    );
    assert!(runner.state().objects[&remover].timestamp < runner.state().objects[&host].timestamp);
    assert_eq!(
        runner.state().objects[&provider].trigger_definitions.len(),
        1
    );
    assert_eq!(
        activated_ability_definitions(runner.state(), provider).len(),
        1
    );
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    // CR 613.6: layer 4 qualifies the original parent and fixes its host
    // recipient. The older layer-6 removal cannot stop its later grants.
    assert!(runner.state().objects[&host]
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert_eq!(activated_ability_definitions(runner.state(), host).len(), 1);
    assert_eq!(
        runner.state().objects[&host].trigger_definitions.len(),
        1,
        "retained parent installs one trigger; the unstarted parent stays suppressed"
    );
    let granted = runner.state().objects[&host]
        .trigger_definitions
        .first()
        .unwrap();
    assert!(matches!(
        &granted.occurrence,
        TriggerDefinitionOccurrenceRef::ExpandedGrant { provider: source, .. }
            if source.source.object_id == provider
    ));
    let life_before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    // The provider's printed trigger and the host's granted trigger each gain 2 life.
    assert_eq!(runner.life(P0), life_before + 4);
}

#[test]
fn distinct_granted_static_generators_order_a_provider_writer_before_its_reader() {
    let mut scenario = GameScenario::new();
    let first_granter = scenario.add_creature(P0, "First Granter", 1, 1).id();
    let recipient = scenario.add_creature(P0, "Recipient", 1, 1).id();
    let donor = scenario.add_creature(P0, "Donor", 1, 1).id();
    let second_granter = scenario.add_creature(P0, "Second Granter", 1, 1).id();
    let mut runner = scenario.build();
    let donated_ability =
        AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
    let inner_reader = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
            source: TargetFilter::SpecificObject { id: donor },
            cap: None,
        }]);
    let inner_writer = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: donor })
        .modifications(vec![ContinuousModification::GrantAbility {
            definition: Box::new(donated_ability.clone()),
        }]);
    attach_synthetic_static(
        &mut runner,
        first_granter,
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: recipient })
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(inner_reader.clone()),
            }]),
    );
    attach_synthetic_static(
        &mut runner,
        second_granter,
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: recipient })
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(inner_writer.clone()),
            }]),
    );
    assert!(
        runner.state().objects[&first_granter].timestamp
            < runner.state().objects[&recipient].timestamp
    );
    assert!(
        runner.state().objects[&recipient].timestamp < runner.state().objects[&donor].timestamp
    );
    assert!(
        runner.state().objects[&donor].timestamp
            < runner.state().objects[&second_granter].timestamp
    );
    assert!(activated_ability_definitions(runner.state(), donor).is_empty());

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    // CR 613.8a-b: The later writer changes the older reader's donor output,
    // even though both statics were granted to the same recipient.
    let granted_statics = &runner.state().objects[&recipient].static_definitions;
    assert!(granted_statics
        .iter_unchecked()
        .any(|definition| definition == &inner_reader));
    assert!(granted_statics
        .iter_unchecked()
        .any(|definition| definition == &inner_writer));
    let donor_abilities = activated_ability_definitions(runner.state(), donor);
    assert_eq!(
        donor_abilities.len(),
        1,
        "the second grant must reach the donor"
    );
    assert_eq!(donor_abilities[0].1, donated_ability);
    let recipient_abilities = activated_ability_definitions(runner.state(), recipient);
    assert_eq!(
        recipient_abilities.len(),
        1,
        "the recipient must copy the donor ability"
    );
    assert_eq!(recipient_abilities[0].1, donated_ability);
}

#[test]
fn nested_meta_grants_read_original_static_granter_suppression() {
    // Synthetic typed fixture: no printed Oracle premise is asserted here.
    // The two runs distinguish an unstarted inner static from one whose
    // earlier-layer part has already fixed its recipient under CR 613.6.
    for retained_in_type_layer in [false, true] {
        let mut scenario = GameScenario::new();
        let granter = scenario.add_creature(P0, "Synthetic Granter", 1, 1).id();
        let recipient = scenario.add_creature(P0, "Synthetic Recipient", 1, 1).id();
        let trigger = TriggerDefinition::new(TriggerMode::Phase)
            .phase(Phase::Upkeep)
            .trigger_zones(vec![Zone::Battlefield])
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            ));
        let provider = scenario
            .add_creature(P0, "Synthetic Provider", 1, 1)
            .with_trigger_definition(trigger)
            .with_ability_definition(
                AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap),
            )
            .id();
        let remover = scenario.add_creature(P0, "Synthetic Remover", 1, 1).id();
        let mut runner = scenario.build();
        let mut inner_modifications = Vec::new();
        if retained_in_type_layer {
            inner_modifications.push(ContinuousModification::AddType {
                core_type: CoreType::Artifact,
            });
        }
        inner_modifications.extend([
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::SpecificObject { id: provider },
                cap: None,
            },
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::SpecificObject { id: provider },
            },
        ]);
        attach_synthetic_static(
            &mut runner,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: recipient })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(
                        StaticDefinition::continuous()
                            .affected(TargetFilter::SelfRef)
                            .modifications(inner_modifications),
                    ),
                }]),
        );
        assert!(
            runner.state().objects[&granter].timestamp < runner.state().objects[&remover].timestamp
        );
        assert_eq!(
            activated_ability_definitions(runner.state(), provider).len(),
            1
        );
        assert_eq!(
            runner.state().objects[&provider].trigger_definitions.len(),
            1
        );
        mark_layers_full(runner.state_mut());
        flush_layers(runner.state_mut());

        // CR 113.3d + CR 613.1f: the original granter reaches the recipient,
        // and that recipient reads both exact definitions from the provider.
        assert!(runner.state().objects[&recipient]
            .static_definitions
            .iter_unchecked()
            .any(
                |definition| definition.modifications.iter().any(|modification| matches!(
                    modification,
                    ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
                ))
            ));
        assert_eq!(
            activated_ability_definitions(runner.state(), recipient).len(),
            1
        );
        let granted_trigger = &runner.state().objects[&recipient].trigger_definitions;
        assert_eq!(granted_trigger.len(), 1);
        assert!(matches!(
            &granted_trigger[0].occurrence,
            TriggerDefinitionOccurrenceRef::ExpandedGrant { provider: source, .. }
                if source.source.object_id == provider
        ));

        attach_synthetic_static(
            &mut runner,
            remover,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: granter })
                .modifications(vec![ContinuousModification::RemoveAllAbilities]),
        );
        mark_layers_full(runner.state_mut());
        flush_layers(runner.state_mut());
        assert!(runner.state().objects[&granter]
            .static_definitions
            .is_empty());
        assert_eq!(
            activated_ability_definitions(runner.state(), provider).len(),
            1
        );
        assert_eq!(
            runner.state().objects[&provider].trigger_definitions.len(),
            1
        );

        if retained_in_type_layer {
            // CR 613.6: the inner static began in layer 4, so ability removal
            // cannot stop its later layer-6 grants to the retained recipient.
            assert!(runner.state().objects[&recipient]
                .card_types
                .core_types
                .contains(&CoreType::Artifact));
            assert_eq!(
                activated_ability_definitions(runner.state(), recipient).len(),
                1
            );
            assert_eq!(
                runner.state().objects[&recipient].trigger_definitions.len(),
                1
            );
        } else {
            // CR 613.8a-c: removal of the original granter precedes either
            // unstarted nested reader. These assertions flip if the original
            // granter is omitted from the dependency reach check.
            assert!(activated_ability_definitions(runner.state(), recipient).is_empty());
            assert!(runner.state().objects[&recipient]
                .trigger_definitions
                .is_empty());
        }
    }
}

#[test]
fn inactive_original_grant_cannot_start_an_earlier_inner_type_part() {
    let mut scenario = GameScenario::new();
    let granter = scenario.add_creature(P0, "Turn Granter", 1, 1).id();
    let carrier = scenario.add_creature(P0, "Carrier", 1, 1).id();
    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
    let donor = scenario
        .add_creature(P0, "Donor", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap),
        )
        .with_trigger_definition(trigger)
        .id();
    let mut runner = scenario.build();
    let inner = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::AddType {
                core_type: CoreType::Artifact,
            },
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::SpecificObject { id: donor },
                cap: None,
            },
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::SpecificObject { id: donor },
            },
        ]);
    attach_synthetic_static(
        &mut runner,
        granter,
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: carrier })
            .condition(StaticCondition::DuringYourTurn)
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(inner.clone()),
            }]),
    );
    assert!(runner.state().objects[&granter]
        .static_definitions
        .iter_unchecked()
        .any(|definition| matches!(
            definition.modifications.as_slice(),
            [ContinuousModification::GrantStaticAbility { .. }]
        )));
    assert_eq!(
        activated_ability_definitions(runner.state(), donor).len(),
        1
    );
    assert_eq!(runner.state().objects[&donor].trigger_definitions.len(), 1);

    runner.state_mut().active_player = P1;
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(!runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &inner));
    assert!(!runner.state().objects[&carrier]
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert!(activated_ability_definitions(runner.state(), carrier).is_empty());
    assert!(runner.state().objects[&carrier]
        .trigger_definitions
        .is_empty());

    runner.state_mut().active_player = P0;
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &inner));
    assert!(runner.state().objects[&carrier]
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert_eq!(
        activated_ability_definitions(runner.state(), carrier).len(),
        1
    );
    assert!(matches!(
        &runner.state().objects[&carrier].trigger_definitions[0].occurrence,
        TriggerDefinitionOccurrenceRef::ExpandedGrant { provider, .. }
            if provider.source.object_id == donor
    ));
}

#[test]
fn original_grant_qualifies_carrier_independently_of_inner_target_and_controller() {
    let mut scenario = GameScenario::new();
    let first_granter = scenario.add_creature(P0, "First Granter", 1, 1).id();
    let second_granter = scenario.add_creature(P1, "Second Granter", 1, 1).id();
    let carrier = scenario.add_creature(P1, "Carrier", 1, 1).id();
    let target = scenario.add_creature(P1, "Target", 1, 1).id();
    let first_ability =
        AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
    let second_ability = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
    let first_donor = scenario
        .add_creature(P0, "First Donor", 1, 1)
        .with_ability_definition(first_ability.clone())
        .with_trigger_definition(trigger.clone())
        .id();
    let second_donor = scenario
        .add_creature(P1, "Second Donor", 1, 1)
        .with_ability_definition(second_ability.clone())
        .with_trigger_definition(trigger)
        .id();
    let mut runner = scenario.build();
    let first_inner = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: target })
        .condition(StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn),
        })
        .modifications(vec![
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::SpecificObject { id: first_donor },
                cap: None,
            },
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::SpecificObject { id: first_donor },
            },
        ]);
    let second_inner = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: target })
        .condition(StaticCondition::DuringYourTurn)
        .modifications(vec![
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::SpecificObject { id: second_donor },
                cap: None,
            },
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::SpecificObject { id: second_donor },
            },
        ]);
    for (granter, inner) in [
        (first_granter, &first_inner),
        (second_granter, &second_inner),
    ] {
        attach_synthetic_static(
            &mut runner,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: carrier })
                .condition(StaticCondition::DuringYourTurn)
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new((*inner).clone()),
                }]),
        );
        assert!(runner.state().objects[&granter]
            .static_definitions
            .iter_unchecked()
            .any(|definition| matches!(
                definition.modifications.as_slice(),
                [ContinuousModification::GrantStaticAbility { .. }]
            )));
    }
    assert_eq!(
        activated_ability_definitions(runner.state(), first_donor)[0].1,
        first_ability
    );
    assert_eq!(
        activated_ability_definitions(runner.state(), second_donor)[0].1,
        second_ability
    );
    assert_eq!(
        runner.state().objects[&first_donor]
            .trigger_definitions
            .len(),
        1
    );
    assert_eq!(
        runner.state().objects[&second_donor]
            .trigger_definitions
            .len(),
        1
    );

    runner.state_mut().active_player = P0;
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &first_inner));
    assert!(!runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &second_inner));
    assert_eq!(
        activated_ability_definitions(runner.state(), target)[0].1,
        first_ability
    );
    assert!(activated_ability_definitions(runner.state(), carrier).is_empty());
    assert!(matches!(
        &runner.state().objects[&target].trigger_definitions[0].occurrence,
        TriggerDefinitionOccurrenceRef::ExpandedGrant { provider, .. }
            if provider.source.object_id == first_donor
    ));

    runner.state_mut().active_player = P1;
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(!runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &first_inner));
    assert!(runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &second_inner));
    assert_eq!(
        activated_ability_definitions(runner.state(), target)[0].1,
        second_ability
    );
    assert!(activated_ability_definitions(runner.state(), carrier).is_empty());
    assert!(matches!(
        &runner.state().objects[&target].trigger_definitions[0].occurrence,
        TriggerDefinitionOccurrenceRef::ExpandedGrant { provider, .. }
            if provider.source.object_id == second_donor
    ));
}

#[test]
fn outside_slice_granted_static_carrier_escalates_only_for_reachable_entrant() {
    let mut scenario = GameScenario::new();
    let granter = scenario.add_creature(P0, "Granter", 1, 1).id();
    let carrier = scenario.add_creature(P0, "Carrier", 1, 1).id();
    let target = scenario.add_creature_to_graveyard(P0, "Target", 1, 1).id();
    let irrelevant = scenario
        .add_creature_to_graveyard(P0, "Irrelevant", 1, 1)
        .id();
    let ability =
        AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
    let donor = scenario
        .add_creature(P0, "Donor", 1, 1)
        .with_ability_definition(ability.clone())
        .id();
    let mut runner = scenario.build();
    let inner = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: target })
        .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
            source: TargetFilter::SpecificObject { id: donor },
            cap: None,
        }]);
    attach_synthetic_static(
        &mut runner,
        granter,
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: carrier })
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(inner.clone()),
            }]),
    );
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(runner.state().objects[&granter]
        .static_definitions
        .iter_unchecked()
        .any(|definition| matches!(
            definition.modifications.as_slice(),
            [ContinuousModification::GrantStaticAbility { .. }]
        )));
    assert!(runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &inner));
    assert_eq!(
        activated_ability_definitions(runner.state(), donor)[0].1,
        ability
    );

    move_to_zone(
        runner.state_mut(),
        irrelevant,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(
        runner.state().layers_dirty,
        LayersDirty::EnteredObjects([irrelevant].into())
    );
    engine::game::perf_counters::reset();
    flush_layers(runner.state_mut());
    let counters = engine::game::perf_counters::snapshot();
    assert_eq!(counters.layers_incremental, 1);
    assert_eq!(counters.layers_escalated, 0);
    assert!(runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &inner));

    move_to_zone(
        runner.state_mut(),
        target,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(
        runner.state().layers_dirty,
        LayersDirty::EnteredObjects([target].into())
    );
    engine::game::perf_counters::reset();
    flush_layers(runner.state_mut());
    let counters = engine::game::perf_counters::snapshot();
    assert_eq!(counters.layers_escalated, 1);
    assert_eq!(counters.layers_incremental, 0);
    assert_eq!(
        activated_ability_definitions(runner.state(), target)[0].1,
        ability
    );
}

#[test]
fn freshly_entering_carrier_uses_restricted_parent_qualification() {
    let mut scenario = GameScenario::new();
    let granter = scenario.add_creature(P0, "Granter", 1, 1).id();
    let carrier = scenario
        .add_creature_to_graveyard(P0, "Fresh Carrier", 1, 1)
        .id();
    let ability =
        AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
    let donor = scenario
        .add_creature(P0, "Donor", 1, 1)
        .with_ability_definition(ability.clone())
        .id();
    let mut runner = scenario.build();
    let inner = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
            source: TargetFilter::SpecificObject { id: donor },
            cap: None,
        }]);
    attach_synthetic_static(
        &mut runner,
        granter,
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: carrier })
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(inner.clone()),
            }]),
    );
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert_eq!(
        activated_ability_definitions(runner.state(), donor)[0].1,
        ability
    );
    move_to_zone(
        runner.state_mut(),
        carrier,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(
        runner.state().layers_dirty,
        LayersDirty::EnteredObjects([carrier].into())
    );
    engine::game::perf_counters::reset();
    flush_layers(runner.state_mut());
    let counters = engine::game::perf_counters::snapshot();
    assert_eq!(counters.layers_incremental, 1);
    assert!(runner.state().objects[&carrier]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &inner));
    assert_eq!(
        activated_ability_definitions(runner.state(), carrier)[0].1,
        ability
    );
}

#[test]
fn remote_mana_ability_writer_enables_exact_original_grant_before_nested_reader() {
    for writer_reaches_witness in [true, false] {
        let mut scenario = GameScenario::new();
        let granter = scenario.add_creature(P0, "Granter", 1, 1).id();
        let carrier = scenario.add_creature(P0, "Carrier", 1, 1).id();
        let witness = scenario.add_creature(P0, "Witness", 1, 1).id();
        let writer = scenario.add_creature(P0, "Writer", 1, 1).id();
        let other = scenario.add_creature(P0, "Other", 1, 1).id();
        let donor_ability =
            AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
        let donor = scenario
            .add_creature(P0, "Donor", 1, 1)
            .with_ability_definition(donor_ability.clone())
            .id();
        let mana_template = scenario
            .add_artifact_from_oracle(P0, "Mana Template", SOL_RING)
            .id();
        let mut runner = scenario.build();
        let mana_ability = activated_ability_definitions(runner.state(), mana_template)[0]
            .1
            .clone();
        let inner = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::SpecificObject { id: donor },
                cap: None,
            }]);
        attach_synthetic_static(
            &mut runner,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: carrier })
                .condition(StaticCondition::IsPresent {
                    filter: Some(TargetFilter::And {
                        filters: vec![
                            TargetFilter::SpecificObject { id: witness },
                            TargetFilter::Typed(
                                TypedFilter::default().properties(vec![FilterProp::HasManaAbility]),
                            ),
                        ],
                    }),
                })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(inner.clone()),
                }]),
        );
        attach_synthetic_static(
            &mut runner,
            writer,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject {
                    id: if writer_reaches_witness {
                        witness
                    } else {
                        other
                    },
                })
                .modifications(vec![ContinuousModification::GrantAbility {
                    definition: Box::new(mana_ability.clone()),
                }]),
        );
        assert!(
            runner.state().objects[&granter].timestamp < runner.state().objects[&carrier].timestamp
        );
        assert!(
            runner.state().objects[&carrier].timestamp < runner.state().objects[&witness].timestamp
        );
        assert!(
            runner.state().objects[&witness].timestamp < runner.state().objects[&writer].timestamp
        );
        assert!(activated_ability_definitions(runner.state(), witness).is_empty());
        assert_eq!(
            activated_ability_definitions(runner.state(), donor)[0].1,
            donor_ability
        );
        assert!(runner.state().objects[&granter]
            .static_definitions
            .iter_unchecked()
            .any(|definition| matches!(
                definition.modifications.as_slice(),
                [ContinuousModification::GrantStaticAbility { .. }]
            )));
        assert!(runner.state().objects[&writer]
            .static_definitions
            .iter_unchecked()
            .any(|definition| matches!(
                definition.modifications.as_slice(),
                [ContinuousModification::GrantAbility { .. }]
            )));

        mark_layers_full(runner.state_mut());
        flush_layers(runner.state_mut());
        let written = if writer_reaches_witness {
            witness
        } else {
            other
        };
        assert_eq!(
            activated_ability_definitions(runner.state(), written)[0].1,
            mana_ability
        );
        let carrier_has_inner = runner.state().objects[&carrier]
            .static_definitions
            .iter_unchecked()
            .any(|definition| definition == &inner);
        assert_eq!(carrier_has_inner, writer_reaches_witness);
        if writer_reaches_witness {
            assert_eq!(
                activated_ability_definitions(runner.state(), carrier)[0].1,
                donor_ability
            );
        } else {
            assert!(activated_ability_definitions(runner.state(), witness).is_empty());
            assert!(activated_ability_definitions(runner.state(), carrier).is_empty());
        }
    }
}

#[test]
fn ordinary_granted_static_keeps_keyword_filter_dependency_in_referenced_bucket() {
    let mut scenario = GameScenario::new();
    let granter = scenario.add_creature(P0, "Ordinary Granter", 1, 1).id();
    let recipient = scenario.add_creature(P0, "Recipient", 1, 1).id();
    let writer = scenario.add_creature(P0, "Flying Writer", 1, 1).id();
    let meta_host = scenario.add_creature(P0, "Referenced Host", 1, 1).id();
    let donor_ability =
        AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap);
    let donor = scenario
        .add_creature(P0, "Distinct Donor", 1, 1)
        .with_ability_definition(donor_ability.clone())
        .id();
    let mut runner = scenario.build();
    let ordinary_inner = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance,
        }]);
    attach_synthetic_static(
        &mut runner,
        granter,
        StaticDefinition::continuous()
            .affected(TargetFilter::And {
                filters: vec![
                    TargetFilter::SpecificObject { id: recipient },
                    TargetFilter::Typed(TypedFilter::default().properties(vec![
                        FilterProp::WithKeyword {
                            value: Keyword::Flying,
                        },
                    ])),
                ],
            })
            .modifications(vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(ordinary_inner.clone()),
            }]),
    );
    let flying_writer = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: recipient })
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }]);
    attach_synthetic_static(&mut runner, writer, flying_writer.clone());
    attach_synthetic_static(
        &mut runner,
        meta_host,
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::SpecificObject { id: donor },
                cap: None,
            }]),
    );
    assert!(runner.state().objects[&granter].timestamp < runner.state().objects[&writer].timestamp);
    assert!(!runner.state().objects[&recipient].has_keyword(&Keyword::Flying));
    assert!(!runner.state().objects[&recipient]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &ordinary_inner));
    assert!(runner.state().objects[&granter]
        .static_definitions
        .iter_unchecked()
        .any(
            |definition| definition.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantStaticAbility { definition }
                    if definition.as_ref() == &ordinary_inner
            ))
        ));
    assert!(runner.state().objects[&writer]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &flying_writer));
    let donor_abilities = activated_ability_definitions(runner.state(), donor);
    assert_eq!(donor_abilities.len(), 1);
    assert_eq!(donor_abilities[0].1, donor_ability);

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    // CR 613.8a-c + CR 611.3a: the later Flying grant changes which object
    // the ordinary static grant affects, even when another grant reads a donor.
    assert!(runner.state().objects[&recipient].has_keyword(&Keyword::Flying));
    let granted_abilities = activated_ability_definitions(runner.state(), meta_host);
    assert_eq!(granted_abilities.len(), 1);
    assert_eq!(granted_abilities[0].1, donor_ability);
    assert!(runner.state().objects[&recipient]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition == &ordinary_inner));
}
