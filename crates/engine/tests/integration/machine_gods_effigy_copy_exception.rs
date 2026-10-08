//! Regression for Machine God's Effigy.
//!
//! Its copy replacement says `except it's an artifact`, which replaces the
//! copied creature's card types; it is not the additive `in addition to its
//! other types` form used by Copy Artifact and similar cards.

use engine::game::effects::become_copy;
use engine::game::layers::evaluate_layers;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, CopyRecipient, Duration, Effect,
    QuantityExpr, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const MACHINE_GODS_EFFIGY: &str = "You may have this artifact enter as a copy of any creature on the battlefield, except it's an artifact and it has \"{T}: Add {U}.\" (It's not a creature.)\n{T}: Add {U}.";
const COPY_ARTIFACT: &str = "You may have this enchantment enter as a copy of any artifact on the battlefield, except it's an enchantment in addition to its other types.";
const LAZOTEP_CONVERT: &str = "You may have this creature enter as a copy of any creature card in a graveyard, except it's a 4/4 black Zombie in addition to its other colors and types.";
const COLOR_REPLACEMENT_EXCEPTION: &str = "You may have this creature enter as a copy of any creature on the battlefield, except it's a 4/4 white Zombie.";
const DEVOID: &str = "Devoid (This card has no color.)";
const DEVOID_AND_CHANGELING: &str =
    "Devoid (This card has no color.)\nChangeling (This card is every creature type.)";
const CROAKING_COUNTERPART: &str =
    "Create a token that's a copy of target non-Frog creature, except it's a 1/1 green Frog.\nFlashback {3}{G}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const THE_SCARAB_GOD: &str = "At the beginning of your upkeep, each opponent loses X life and you scry X, where X is the number of Zombies you control.\n{2}{U}{B}: Exile target creature card from a graveyard. Create a token that's a copy of it, except it's a 4/4 black Zombie.\nWhen The Scarab God dies, return it to its owner's hand at the beginning of the next end step.";
const SAW_IN_HALF: &str = "Destroy target creature. If that creature dies this way, its controller creates two tokens that are copies of that creature, except their power is half that creature's power and their toughness is half that creature's toughness. Round up each time.";
const TARMOGOYF: &str = "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.";

fn copy_exception_modifications(
    oracle: &str,
    name: &str,
    card_types: &[String],
) -> Vec<ContinuousModification> {
    let parsed = parse_oracle_text(oracle, name, &[], card_types, &[]);
    let replacement = parsed
        .replacements
        .first()
        .expect("copy-as-enters replacement must parse");
    let execute = replacement
        .execute
        .as_ref()
        .expect("copy-as-enters replacement must carry an execute ability");
    let Effect::BecomeCopy {
        additional_modifications,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("replacement must execute BecomeCopy: {execute:?}");
    };
    additional_modifications.clone()
}

fn resolve_self_copy(
    state: &mut engine::types::game_state::GameState,
    recipient: ObjectId,
    donor: ObjectId,
    additional_modifications: Vec<ContinuousModification>,
) {
    let ability = ResolvedAbility::new(
        Effect::BecomeCopy {
            recipient: CopyRecipient::Source,
            target: TargetFilter::Any,
            duration: Some(Duration::Permanent),
            mana_value_limit: None,
            additional_modifications,
        },
        vec![TargetRef::Object(donor)],
        recipient,
        P0,
    );
    become_copy::resolve(state, &ability, &mut Vec::new()).expect("copy resolver succeeds");
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// The exact Oracle parser output used by card-data generation must distinguish
/// type replacement from the additive Copy Artifact family.
#[test]
fn copy_exception_type_modes_remain_distinct() {
    let effigy_modifications = copy_exception_modifications(
        MACHINE_GODS_EFFIGY,
        "Machine God's Effigy",
        &["Artifact".to_string()],
    );
    assert!(
        effigy_modifications.contains(&ContinuousModification::SetCardTypes {
            core_types: vec![CoreType::Artifact],
        })
    );
    assert!(
        effigy_modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::GrantAbility { definition }
                if matches!(definition.effect.as_ref(), Effect::Mana { .. })
        )),
        "Effigy must retain its quoted blue mana ability: {effigy_modifications:?}"
    );
    assert!(
        !effigy_modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::GrantAbility { definition }
                if matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        )),
        "Effigy must have no unimplemented copied exception: {effigy_modifications:?}"
    );

    let copy_artifact_modifications =
        copy_exception_modifications(COPY_ARTIFACT, "Copy Artifact", &["Enchantment".to_string()]);
    assert!(
        copy_artifact_modifications.contains(&ContinuousModification::AddType {
            core_type: CoreType::Enchantment,
        })
    );
    assert!(
        !copy_artifact_modifications
            .iter()
            .any(|modification| matches!(
                modification,
                ContinuousModification::SetCardTypes { .. }
            )),
        "Copy Artifact remains additive: {copy_artifact_modifications:?}"
    );
}

/// CR 614.12a + CR 707.9b + CR 205.1a: selecting the second of two creature
/// donors as Machine God's Effigy enters copies that donor, but the exception
/// replaces Creature with Artifact and grants the blue mana ability.
#[test]
fn effigy_copies_selected_donor_as_a_noncreature_artifact_with_blue_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _first = scenario.add_creature(P0, "First Donor", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Donor", 5, 4).id();
    let effigy = scenario
        .add_artifact_to_hand_from_oracle(P0, "Machine God's Effigy", MACHINE_GODS_EFFIGY)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|index| ManaUnit::new(ManaType::Colorless, ObjectId(9_000 + index), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner
        .cast(effigy)
        .replacement_choice(0)
        .copy_target(second)
        .resolve();

    let copied = &runner.state().objects[&effigy];
    assert_eq!(
        copied.name, "Second Donor",
        "the selected donor must be copied"
    );
    assert_eq!(copied.power, Some(5));
    assert_eq!(copied.toughness, Some(4));
    assert!(copied.card_types.core_types.contains(&CoreType::Artifact));
    assert!(
        !copied.card_types.core_types.contains(&CoreType::Creature),
        "the exact Effigy exception replaces creature with artifact"
    );
    let mana_ability = copied
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("the quoted blue mana ability must be present after copying");

    runner.activate(effigy, mana_ability).resolve();
    let state = runner.state();
    assert!(
        state.objects[&effigy].tapped,
        "the mana ability pays its tap cost"
    );
    assert_eq!(
        state.players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        1
    );
}

/// CR 707.2 + CR 707.9b/d: the completed Effigy copy exception is part of its
/// copiable values.  Copy Artifact therefore sees the artifact-only Effigy,
/// then adds Enchantment without reintroducing Creature.
#[test]
fn copy_artifact_snapshots_effigys_complete_type_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let donor = scenario.add_creature(P0, "Effigy Donor", 5, 4).id();
    let effigy = scenario
        .add_artifact_to_hand_from_oracle(P0, "Machine God's Effigy", MACHINE_GODS_EFFIGY)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let copy_artifact = scenario
        .add_creature_to_hand_from_oracle(P0, "Copy Artifact", 0, 0, COPY_ARTIFACT)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|index| ManaUnit::new(ManaType::Colorless, ObjectId(9_100 + index), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner
        .cast(effigy)
        .replacement_choice(0)
        .copy_target(donor)
        .resolve();
    runner
        .cast(copy_artifact)
        .replacement_choice(0)
        .copy_target(effigy)
        .resolve();

    let copied = &runner.state().objects[&copy_artifact];
    assert!(copied.card_types.core_types.contains(&CoreType::Artifact));
    assert!(copied
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    assert!(
        !copied.card_types.core_types.contains(&CoreType::Creature),
        "Copy Artifact must snapshot Effigy's artifact-only copy exception"
    );
    let mana_ability = copied
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("a copy of Effigy retains its blue mana ability");

    runner.activate(copy_artifact, mana_ability).resolve();
    let state = runner.state();
    assert!(state.objects[&copy_artifact].tapped);
    assert_eq!(
        state.players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        1
    );
}

/// CR 707.9d + CR 604.3: resolver-level regression using Lazotep Convert's
/// exactly parsed copy exception. Its black exception replaces a copied Devoid
/// creature's color-defining ability, even though it adds black in addition to
/// the source's other colors and types.
#[test]
fn lazotep_convert_color_exception_does_not_copy_devoid_cda() {
    let mut scenario = GameScenario::new();
    let donor = {
        let mut builder = scenario.add_creature(P0, "Devoid Donor", 2, 3);
        builder.from_oracle_text_with_keywords(&["Devoid"], DEVOID);
        builder.id()
    };
    let recipient = scenario.add_creature(P0, "Lazotep Host", 0, 0).id();
    let mut state = scenario.build().state().clone();

    assert!(
        state.objects[&donor]
            .base_static_definitions
            .iter()
            .any(|definition| {
                definition.characteristic_defining
                    && matches!(
                        definition.modifications.as_slice(),
                        [ContinuousModification::SetColor { colors }] if colors.is_empty()
                    )
            }),
        "the test donor must carry Devoid's synthesized color CDA"
    );

    let modifications = copy_exception_modifications(
        LAZOTEP_CONVERT,
        "Lazotep Convert",
        &["Creature".to_string()],
    );
    assert!(
        modifications.contains(&ContinuousModification::AddColor {
            color: ManaColor::Black,
        }),
        "Lazotep Convert must reach the folded additive-color exception: {modifications:?}"
    );

    resolve_self_copy(&mut state, recipient, donor, modifications);

    let copied = &state.objects[&recipient];
    assert_eq!(copied.name, "Devoid Donor");
    assert_eq!((copied.power, copied.toughness), (Some(4), Some(4)));
    assert!(copied.card_types.subtypes.contains(&"Zombie".to_string()));
    assert_eq!(copied.color, vec![ManaColor::Black]);
}

/// CR 707.9b/d + CR 604.3: replacing color and creature types are copiable copy
/// exceptions. A later vanilla copy must see the exception's white Zombie values
/// rather than the original Devoid and Changeling CDAs.
#[test]
fn color_and_type_replacement_exception_prunes_cdas_for_later_copies() {
    let mut scenario = GameScenario::new();
    let donor = {
        let mut builder = scenario.add_creature(P0, "Devoid Donor", 2, 3);
        builder.from_oracle_text_with_keywords(&["Devoid", "Changeling"], DEVOID_AND_CHANGELING);
        builder.id()
    };
    let first = scenario.add_creature(P0, "First Host", 0, 0).id();
    let second = scenario.add_creature(P0, "Second Host", 0, 0).id();
    let mut state = scenario.build().state().clone();

    let modifications = copy_exception_modifications(
        COLOR_REPLACEMENT_EXCEPTION,
        "Color Replacement",
        &["Creature".to_string()],
    );
    assert!(
        modifications.contains(&ContinuousModification::SetColor {
            colors: vec![ManaColor::White],
        }),
        "a non-additive color exception must reach the typed SetColor form: {modifications:?}"
    );

    resolve_self_copy(&mut state, first, donor, modifications);
    assert_eq!(state.objects[&first].color, vec![ManaColor::White]);
    assert_eq!(
        state.objects[&first].card_types.subtypes,
        vec!["Zombie"],
        "the replacement must suppress Changeling's type CDA"
    );

    resolve_self_copy(&mut state, second, first, Vec::new());
    assert_eq!(
        state.objects[&second].color,
        vec![ManaColor::White],
        "a later copy must snapshot the color replacement, not Devoid's CDA"
    );
    assert_eq!(
        state.objects[&second].card_types.subtypes,
        vec!["Zombie"],
        "a later copy must snapshot the type replacement, not Changeling's CDA"
    );
}

/// CR 707.9d + CR 604.3: Croaking Counterpart's green copy-token exception
/// supplies color, so a copied Devoid CDA is not part of the token's copied
/// values even beside an unrelated, unclassifiable subtype CDA. A non-Frog
/// Devoid donor is deliberately used: a Changeling is a Frog and therefore
/// illegal for Croaking Counterpart to target.
#[test]
fn croaking_counterpart_copy_token_prunes_devoid_beside_unknown_cda() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = {
        let mut builder = scenario.add_creature(P0, "Devoid Donor", 2, 3);
        builder.from_oracle_text_with_keywords(&["Devoid"], DEVOID);
        builder.with_static_definition(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .cda()
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: "Dog".to_string(),
                }]),
        );
        builder.id()
    };
    let counterpart = scenario
        .add_spell_to_hand_from_oracle(P0, "Croaking Counterpart", false, CROAKING_COUNTERPART)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_200), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_201), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_202), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(9_203), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(counterpart).target_object(donor).resolve();

    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.name == "Devoid Donor")
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "the parsed spell must create one copy token"
    );
    let token = tokens[0];
    assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
    assert_eq!(token.color, vec![ManaColor::Green]);
    assert_eq!(token.card_types.subtypes, vec!["Frog", "Dog"]);
    assert!(
        token.base_static_definitions.iter().any(|definition| {
            definition.characteristic_defining
                && definition.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddSubtype { subtype } if subtype == "Dog"
                    )
                })
        }),
        "the unclassifiable subtype CDA itself remains in the copied token body"
    );
    assert!(
        !token.base_static_definitions.iter().any(|definition| {
            definition.characteristic_defining
                && definition.modifications.iter().any(|modification| {
                    matches!(modification, ContinuousModification::SetColor { .. })
                })
        }),
        "the independent Devoid color CDA must be pruned from the copied token body"
    );
}

/// CR 707.9d + CR 604.3: The Scarab God replaces creature types while making
/// its copy token, so a copied Changeling CDA cannot overwrite the Zombie
/// subtype. Unlike Croaking Counterpart, this real card can legally target a
/// Changeling creature card in a graveyard.
#[test]
fn scarab_god_copy_token_prunes_changeling_type_cda() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = {
        let mut builder = scenario.add_creature_to_graveyard(P0, "Changeling Donor", 2, 3);
        builder.from_oracle_text_with_keywords(&["Changeling"], "Changeling");
        builder.id()
    };
    let scarab_god = scenario
        .add_creature_from_oracle(P0, "The Scarab God", 5, 5, THE_SCARAB_GOD)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_210), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_211), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(9_212), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_213), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner
        .activate(scarab_god, 0)
        .target_object(donor)
        .resolve();

    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.name == "Changeling Donor")
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "the parsed activation must create one copy token"
    );
    let token = tokens[0];
    assert_eq!((token.power, token.toughness), (Some(4), Some(4)));
    assert_eq!(token.color, vec![ManaColor::Black]);
    assert_eq!(token.card_types.subtypes, vec!["Zombie"]);
}

/// CR 707.9d + CR 613.4a/b: Saw in Half's dynamic P/T exception supplies the
/// token's values, so a copied Tarmogoyf P/T CDA cannot reassert itself. Its
/// last-known values before dying are 0/1; once its Creature card reaches a
/// graveyard, a wrongly copied CDA would instead read 1/2. Each token remains
/// the correctly rounded 0/1 copy exception.
#[test]
fn saw_in_half_copy_tokens_prune_dynamic_pt_cda() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tarmogoyf = scenario
        .add_creature_from_oracle(P0, "Tarmogoyf", 0, 1, TARMOGOYF)
        .id();
    let saw_in_half = scenario
        .add_spell_to_hand_from_oracle(P0, "Saw in Half", true, SAW_IN_HALF)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_220), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_221), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_222), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(saw_in_half).target_object(tarmogoyf).resolve();

    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.name == "Tarmogoyf")
        .collect();
    assert_eq!(
        tokens.len(),
        2,
        "the parsed spell must create two copy tokens"
    );
    assert!(
        tokens
            .iter()
            .all(|token| (token.power, token.toughness) == (Some(0), Some(1))),
        "Saw in Half must retain its dynamic 0/1 exception rather than Tarmogoyf's copied 1/2 CDA: {tokens:?}"
    );
}

/// CR 707.9d: CDA pruning is not contingent on being able to fold every copy
/// exception rider. This uses the normal activation/target/stack pipeline for
/// a mixed exception: the name is foldable, while dynamic P/T stays layered.
/// The first copy gets its 1/1 exception; a later vanilla copy cannot inherit
/// Tarmogoyf's P/T CDA, which would otherwise read 1/2 with the staged card in
/// a graveyard.
#[test]
fn mixed_copy_exception_prunes_cda_through_stack_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_spell_to_graveyard(P0, "Evidence", true);
    let donor = scenario
        .add_creature_from_oracle(P0, "Tarmogoyf", 0, 1, TARMOGOYF)
        .id();
    let first = scenario
        .add_creature(P0, "First Mixed Host", 1, 1)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::BecomeCopy {
                recipient: CopyRecipient::Source,
                target: TargetFilter::Typed(TypedFilter::creature()),
                duration: Some(Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::SetName {
                        name: "Mixed Copy".to_string(),
                    },
                    ContinuousModification::SetPowerDynamic {
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    ContinuousModification::SetToughnessDynamic {
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            },
        ))
        .id();
    let second = scenario
        .add_creature(P0, "Second Mixed Host", 1, 1)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::BecomeCopy {
                recipient: CopyRecipient::Source,
                target: TargetFilter::Typed(TypedFilter::creature()),
                duration: Some(Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
        ))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&donor].power,
            runner.state().objects[&donor].toughness,
        ),
        (Some(1), Some(2)),
        "the parsed Tarmogoyf donor's live CDA must establish this regression's distinction"
    );

    runner.activate(first, 0).target_object(donor).resolve();
    assert_eq!(runner.state().objects[&first].name, "Mixed Copy");
    assert_eq!(
        (
            runner.state().objects[&first].power,
            runner.state().objects[&first].toughness,
        ),
        (Some(1), Some(1)),
        "the first copy must retain its layered dynamic P/T exception"
    );

    runner.activate(second, 0).target_object(first).resolve();
    assert_eq!(
        (
            runner.state().objects[&second].power,
            runner.state().objects[&second].toughness,
        ),
        (Some(0), Some(1)),
        "the later vanilla copy must not reacquire the donor's 1/2 Tarmogoyf CDA"
    );
}

/// A non-foldable rider must leave *all* layer operations on the first copy;
/// otherwise the preceding foldable rider leaks into a later vanilla copy.
#[test]
fn unsupported_copy_exception_rider_keeps_preceding_subtype_out_of_later_copy() {
    let mut scenario = GameScenario::new();
    let donor = scenario.add_creature(P0, "Fallback Donor", 2, 2).id();
    let first = scenario.add_creature(P0, "First Host", 0, 0).id();
    let second = scenario.add_creature(P0, "Second Host", 0, 0).id();
    let mut state = scenario.build().state().clone();

    resolve_self_copy(
        &mut state,
        first,
        donor,
        vec![
            ContinuousModification::AddSubtype {
                subtype: "Dog".to_string(),
            },
            ContinuousModification::AddPower { value: 3 },
        ],
    );
    assert!(state.objects[&first]
        .card_types
        .subtypes
        .contains(&"Dog".to_string()));
    assert_eq!(state.objects[&first].power, Some(5));

    resolve_self_copy(&mut state, second, first, Vec::new());
    assert!(
        !state.objects[&second]
            .card_types
            .subtypes
            .contains(&"Dog".to_string()),
        "the unsupported rider must prevent an earlier subtype fold from leaking"
    );
    assert_eq!(state.objects[&second].power, Some(2));
}

/// CR 707.9d: an unclassifiable CDA stays copiable, but it must not prevent a
/// separate, classifiable P/T CDA from being pruned by the same exception.
#[test]
fn unclassifiable_cda_does_not_preserve_an_independent_overridden_cda() {
    let mut scenario = GameScenario::new();
    let donor = scenario.add_creature(P0, "CDA Donor", 2, 2).id();
    let first = scenario.add_creature(P0, "First CDA Host", 0, 0).id();
    let second = scenario.add_creature(P0, "Second CDA Host", 0, 0).id();
    let mut state = scenario.build().state().clone();

    let dog_cda = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .cda()
        .modifications(vec![ContinuousModification::AddSubtype {
            subtype: "Dog".to_string(),
        }]);
    let dynamic_power_cda = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .cda()
        .modifications(vec![ContinuousModification::SetDynamicPower {
            value: QuantityExpr::Fixed { value: 9 },
        }]);
    let donor_object = state
        .objects
        .get_mut(&donor)
        .expect("scenario donor is on the battlefield");
    donor_object.static_definitions = vec![dog_cda.clone(), dynamic_power_cda.clone()].into();
    donor_object.base_static_definitions = std::sync::Arc::new(vec![dog_cda, dynamic_power_cda]);
    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);

    resolve_self_copy(
        &mut state,
        first,
        donor,
        vec![ContinuousModification::SetPower { value: 7 }],
    );
    assert!(state.objects[&first]
        .card_types
        .subtypes
        .contains(&"Dog".to_string()));
    assert_eq!(state.objects[&first].power, Some(7));

    resolve_self_copy(&mut state, second, first, Vec::new());
    assert!(state.objects[&second]
        .card_types
        .subtypes
        .contains(&"Dog".to_string()));
    assert_eq!(
        state.objects[&second].power,
        Some(2),
        "the unknown subtype CDA must not preserve the separate overridden 9-power CDA"
    );
}

/// Resolution-time exceptions are consumed independently of snapshot folding.
/// The first copy receives each exception, while the later vanilla copy sees
/// only the permanent no-cost and starting-loyalty values.
#[test]
fn resolution_time_copy_exceptions_survive_layered_fallback_without_leaking_riders() {
    let mut scenario = GameScenario::new();
    let donor = scenario
        .add_creature(P0, "Resolution Donor", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let first = scenario
        .add_creature(P0, "First Resolution Host", 0, 0)
        .id();
    let second = scenario
        .add_creature(P0, "Second Resolution Host", 0, 0)
        .id();
    let mut state = scenario.build().state().clone();
    let charge = CounterType::Generic("charge".to_string());

    resolve_self_copy(
        &mut state,
        first,
        donor,
        vec![
            ContinuousModification::RemoveManaCost,
            ContinuousModification::SetStartingLoyalty { value: 7 },
            ContinuousModification::AddCounterOnEnter {
                counter_type: charge.clone(),
                count: QuantityExpr::Fixed { value: 1 },
                if_type: Some(CoreType::Creature),
            },
            ContinuousModification::AddPower { value: 3 },
        ],
    );
    assert_eq!(state.objects[&first].mana_cost, ManaCost::NoCost);
    assert_eq!(state.objects[&first].loyalty, Some(7));
    assert_eq!(state.objects[&first].power, Some(5));
    assert_eq!(state.objects[&first].counters.get(&charge), Some(&1));

    resolve_self_copy(&mut state, second, first, Vec::new());
    assert_eq!(state.objects[&second].mana_cost, ManaCost::NoCost);
    assert_eq!(state.objects[&second].loyalty, Some(7));
    assert_eq!(state.objects[&second].power, Some(2));
    assert_eq!(state.objects[&second].counters.get(&charge), None);
}
