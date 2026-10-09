//! Runtime coverage for "becomes the creature type / basic land type of your
//! choice" (CR 205.1a + CR 205.1b + CR 305.7 + CR 608.2d + CR 608.2h).
//!
//! Two seams, one file:
//!
//! * **U1 — the resolution-time latch.** These choosers are `persist: false`,
//!   so their answer lives only in `state.last_named_choice` while the
//!   type-changing `GenericEffect` continuation drains. The layer applier reads
//!   the SOURCE's `chosen_attributes`, which such a choice never writes, so the
//!   change used to apply nothing. `effects/effect.rs::snapshot_transient_modifications`
//!   now latches this resolution's answer (`choose::resolution_chosen_subtype`)
//!   into a fixed `AddSubtype` / `SetBasicLandType` payload ONCE, when the
//!   effect is applied (CR 608.2h), so each resolution keeps its own answer.
//! * **U2 — set versus retain.** Without "in addition to its other types" the
//!   chosen creature type replaces the object's creature types (CR 205.1a) and
//!   the chosen basic land type sets the land's type (CR 305.7); with the
//!   marker the chosen subtype is added (CR 205.1b, CR 305.7).
//!
//! Built on VERBATIM Oracle text (Scryfall). Every negative assertion is paired
//! with a positive reach-guard in the same test.
//!
//! FOOT-GUN: `layers.rs`'s `RemoveAllSubtypes { SubtypeSet::Creature }` arm
//! retains any subtype NOT in `state.all_creature_types`, and `GameScenario`
//! leaves that vector EMPTY — each creature-type test seeds it.
//!
//! REVERT DISCRIMINATORS:
//! * every test here — revert the U1 latch in
//!   `effects/effect.rs::snapshot_transient_modifications` and no chosen subtype
//!   is ever applied (the positive "has Elf" / "has Island" rows flip).
//! * `mistform_stalker_becomes_only_the_chosen_creature_type` /
//!   `mistform_wall_loses_defender_after_becoming_an_elf` — revert U2's
//!   creature set form in `subject.rs::try_parse_become_choice` and the printed
//!   Illusion / Wall survives (the "not Illusion" / "no Defender" rows flip).
//! * `jinx_sets_target_land_to_only_the_chosen_basic_type` — revert U2's land
//!   set form and the Forest type and its green mana ability survive.
//! * `mistform_sliver_two_activations_keep_both_chosen_types` /
//!   `navigators_compass_adds_chosen_basic_type_in_addition` — guard the
//!   retain form: a set form here would drop Elf / Forest.
//! * `unnatural_selection_two_targets_keep_their_own_types` — two live effects
//!   from one source on two recipients each keep their own answer.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityDefinition, ChoiceType, ChosenSubtypeKind, ContinuousModification, ControllerRef,
    Duration, Effect, ManaProduction, QuantityExpr, TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::SubtypeSet;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Terraformer {2}{U} Creature — Human Wizard 2/2 — verbatim.
const TERRAFORMER: &str =
    "{1}: Choose a basic land type. Each land you control becomes that type until end of turn.";

/// Elsewhere Flask {2} Artifact — verbatim, including its independent ETB.
const ELSEWHERE_FLASK: &str =
    "When this artifact enters, draw a card.\nSacrifice this artifact: Choose a basic land type. \
     Each land you control becomes that type until end of turn.";

/// Mistform Wakecaster {4}{U} Creature — Illusion 2/3 — verbatim.
const MISTFORM_WAKECASTER: &str =
    "Flying\n{1}: This creature becomes the creature type of your choice until end of turn.\n\
     {2}{U}{U}, {T}: Choose a creature type. Each creature you control becomes that type until \
     end of turn.";

/// Mistform Stalker {1}{U} Creature — Illusion 1/1 — verbatim.
const MISTFORM_STALKER: &str =
    "{1}: This creature becomes the creature type of your choice until end of turn.\n{2}{U}{U}: \
     This creature gets +2/+2 and gains flying until end of turn.";

/// Mistform Wall {2}{U} Creature — Illusion Wall 1/4 — verbatim.
const MISTFORM_WALL: &str =
    "This creature has defender as long as it's a Wall.\n{1}: This creature becomes the creature \
     type of your choice until end of turn.";

/// Mistform Sliver {1}{U} Creature — Illusion Sliver 1/1 — verbatim.
const MISTFORM_SLIVER: &str =
    "All Slivers have \"{1}: This permanent becomes the creature type of your choice in addition \
     to its other types until end of turn.\"";

/// Unnatural Selection {1}{U} Enchantment — verbatim.
const UNNATURAL_SELECTION: &str =
    "{1}: Choose a creature type other than Wall. Target creature becomes that type until end of \
     turn.";

/// Jinx {1}{U} Instant — verbatim.
const JINX: &str =
    "Target land becomes the basic land type of your choice until end of turn.\nDraw a card at the \
     beginning of the next turn's upkeep.";

/// Navigator's Compass {1} Artifact — verbatim.
const NAVIGATORS_COMPASS: &str =
    "When this artifact enters, you gain 3 life.\n{T}: Until end of turn, target land you control \
     becomes the basic land type of your choice in addition to its other types.";

/// `n` units of blue mana with no producing source — pays `{1}` exactly.
fn blue_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]); n]
}

/// Every creature type these tests remove or offer (see the module FOOT-GUN).
fn seed_creature_types(runner: &mut GameRunner) {
    let types = ["Illusion", "Wall", "Sliver", "Bear", "Elf", "Goblin"];
    runner.state_mut().all_creature_types = types.iter().map(|t| t.to_string()).collect();
}

/// CR 613.1: re-derive every characteristic from the printed base plus all
/// live continuous effects before a read-back.
fn relayer(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn has_subtype(runner: &GameRunner, id: ObjectId, subtype: &str) -> bool {
    runner.state().objects[&id]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == subtype)
}

fn subtypes(runner: &GameRunner, id: ObjectId) -> Vec<String> {
    runner.state().objects[&id].card_types.subtypes.clone()
}

/// Whether the object carries a `{T}: Add [color]` mana ability (CR 305.6
/// intrinsic or printed — both are `Effect::Mana` with a fixed single colour).
fn has_mana_ability_for(runner: &GameRunner, id: ObjectId, color: ManaColor) -> bool {
    runner.state().objects[&id].abilities.iter().any(|ability| {
        matches!(
            &*ability.effect,
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, .. },
                ..
            } if colors.as_slice() == [color]
        )
    })
}

/// Index of the object's ability whose top-level effect is a `Choose` of the
/// given kind (the printed or granted "of your choice" / "choose a" ability).
fn choose_ability_index(
    runner: &GameRunner,
    id: ObjectId,
    is_kind: impl Fn(&ChoiceType) -> bool,
) -> Option<usize> {
    let object = &runner.state().objects[&id];
    object.abilities.iter().position(|ability| {
        matches!(&*ability.effect, Effect::Choose { choice_type, .. } if is_kind(choice_type))
    })
}

fn is_creature_type_choice(choice_type: &ChoiceType) -> bool {
    matches!(choice_type, ChoiceType::CreatureType { .. })
}

fn is_basic_land_type_choice(choice_type: &ChoiceType) -> bool {
    matches!(choice_type, ChoiceType::BasicLandType)
}

fn parse_supported(oracle: &str, name: &str, core_type: &str) -> ParsedAbilities {
    parse_supported_with_keywords(oracle, name, core_type, &[])
}

fn parse_supported_with_keywords(
    oracle: &str,
    name: &str,
    core_type: &str,
    keywords: &[String],
) -> ParsedAbilities {
    let parsed = parse_oracle_text(oracle, name, keywords, &[core_type.to_string()], &[]);
    let debug = format!("{parsed:#?}");
    assert!(!debug.contains("Unimplemented"), "{name}: {debug}");
    parsed
}

fn replacement_modifications(ability: &AbilityDefinition) -> &[ContinuousModification] {
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
        ..
    } = &*ability.effect
    else {
        panic!("expected a type-changing continuation: {ability:#?}");
    };
    // CR 611.2a: the printed until-end-of-turn window survives lowering.
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    assert_eq!(ability.duration, Some(Duration::UntilEndOfTurn));
    assert!(target.is_none(), "these mass changes are untargeted");
    assert_eq!(static_abilities.len(), 1);
    &static_abilities[0].modifications
}

/// SHAPE: the full audited Oracle texts select the existing land replacement
/// operation, while Flask's separate draw trigger remains represented.
#[test]
fn chosen_basic_land_type_full_oracle_shape() {
    for (name, oracle, core_type) in [
        ("Terraformer", TERRAFORMER, "Creature"),
        ("Elsewhere Flask", ELSEWHERE_FLASK, "Artifact"),
    ] {
        let parsed = parse_supported(oracle, name, core_type);
        assert_eq!(parsed.abilities.len(), 1);
        let chooser = &parsed.abilities[0];
        assert!(matches!(
            &*chooser.effect,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                ..
            }
        ));
        let apply = chooser.sub_ability.as_deref().expect("choice continuation");
        assert_eq!(
            replacement_modifications(apply),
            &[ContinuousModification::SetChosenBasicLandType]
        );
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*apply.effect
        else {
            unreachable!();
        };
        assert!(matches!(
            &static_abilities[0].affected,
            Some(TargetFilter::Typed(filter))
                if filter.type_filters == [TypeFilter::Land]
                    && filter.controller == Some(ControllerRef::You)
                    && filter.properties.is_empty()
        ));
        assert!(apply.sub_ability.is_none());
    }
    let flask = parse_supported(ELSEWHERE_FLASK, "Elsewhere Flask", "Artifact");
    assert_eq!(flask.triggers.len(), 1);
    assert!(matches!(
        &*flask.triggers[0]
            .execute
            .as_ref()
            .expect("draw trigger")
            .effect,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller
        }
    ));
}

/// SHAPE: a creature-domain producer selects subtype replacement too. This
/// unrestricted chooser avoids the separate "other than Wall" producer gap.
#[test]
fn chosen_creature_type_full_oracle_shape() {
    let parsed = parse_supported_with_keywords(
        MISTFORM_WAKECASTER,
        "Mistform Wakecaster",
        "Creature",
        &["Flying".into()],
    );
    assert_eq!(parsed.abilities.len(), 2);
    let chooser = &parsed.abilities[1];
    assert!(matches!(
        &*chooser.effect,
        Effect::Choose {
            choice_type: ChoiceType::CreatureType { .. },
            ..
        }
    ));
    assert_eq!(
        replacement_modifications(chooser.sub_ability.as_deref().expect("continuation")),
        &[
            ContinuousModification::RemoveAllSubtypes {
                set: SubtypeSet::Creature
            },
            ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType
            },
        ]
    );
}

/// SHAPE: unsupported anaphors stay explicit gaps. Each valid producer is
/// asserted before the consumer, so an upstream parse failure cannot pass.
#[test]
fn that_type_requires_a_same_chain_subtype_producer() {
    for (oracle, choices) in [
        ("{1}: Each land you control becomes that type until end of turn.", 0),
        ("{1}: Choose a color. Each land you control becomes that type until end of turn.", 1),
        ("{1}: Choose a basic land type. Choose a color. Each land you control becomes that type until end of turn.", 2),
    ] {
        let parsed = parse_oracle_text(oracle, "Choice fixture", &[], &["Creature".into()], &[]);
        assert_eq!(parsed.abilities.len(), 1);
        let mut consumer = &parsed.abilities[0];
        for _ in 0..choices {
            assert!(matches!(&*consumer.effect, Effect::Choose { .. }));
            consumer = consumer.sub_ability.as_deref().expect("next instruction");
        }
        assert!(matches!(
            &*consumer.effect,
            Effect::Unimplemented { name, .. } if name == "chosen_subtype_context"
        ), "{oracle}: {consumer:#?}");
    }
    let parsed = parse_oracle_text(
        "{1}: Choose a basic land type.\n{2}: Each land you control becomes that type until end of turn.",
        "Independent choice fixture", &[], &["Creature".into()], &[],
    );
    assert_eq!(parsed.abilities.len(), 2);
    assert!(matches!(
        &*parsed.abilities[0].effect,
        Effect::Choose {
            choice_type: ChoiceType::BasicLandType,
            ..
        }
    ));
    assert!(
        matches!(&*parsed.abilities[1].effect, Effect::Unimplemented { name, .. } if name == "chosen_subtype_context")
    );
}

/// SHAPE: the nearest producer, rather than the recipient's card type, owns
/// the domain. These are deliberately synthetic composition fixtures.
#[test]
fn that_type_uses_the_nearest_choice_domain() {
    for (first, second, expected) in [
        (
            "creature type",
            "basic land type",
            vec![ContinuousModification::SetChosenBasicLandType],
        ),
        (
            "basic land type",
            "creature type",
            vec![
                ContinuousModification::RemoveAllSubtypes {
                    set: SubtypeSet::Creature,
                },
                ContinuousModification::AddChosenSubtype {
                    kind: ChosenSubtypeKind::CreatureType,
                },
            ],
        ),
    ] {
        let oracle = format!("{{1}}: Choose a {first}. Choose a {second}. Each land you control becomes that type until end of turn.");
        let parsed = parse_supported(&oracle, "Two-domain fixture", "Creature");
        assert_eq!(parsed.abilities.len(), 1);
        let first = &parsed.abilities[0];
        assert!(matches!(&*first.effect, Effect::Choose { .. }));
        let second = first.sub_ability.as_deref().expect("second choice");
        assert!(matches!(&*second.effect, Effect::Choose { .. }));
        let apply = second.sub_ability.as_deref().expect("consumer");
        assert_eq!(replacement_modifications(apply), expected.as_slice());
    }
}

/// CR 305.7 + CR 305.6: setting a chosen basic land type removes the old
/// land subtype and printed abilities, and supplies working intrinsic mana.
#[test]
fn terraformer_replaces_land_types_printed_abilities_and_mana() {
    parse_supported(TERRAFORMER, "Terraformer", "Creature");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terraformer = scenario
        .add_creature_from_oracle(P0, "Terraformer", 2, 2, TERRAFORMER)
        .id();
    // Synthetic support land: both mana and a non-mana printed ability must
    // be replaced. The type-changing ability itself uses verbatim Oracle.
    let forest = scenario
        .add_land_from_oracle(
            P0,
            "Ability-bearing Forest",
            "{T}: Add {G}.\n{T}: You gain 1 life.",
        )
        .with_subtypes(vec!["Forest"])
        .id();
    let opposing_forest = scenario.add_basic_land(P1, ManaColor::Green);
    scenario.with_mana_pool(P0, blue_mana(1));
    let mut runner = scenario.build();
    relayer(&mut runner);
    assert!(has_subtype(&runner, forest, "Forest"));
    assert!(has_mana_ability_for(&runner, forest, ManaColor::Green));
    assert!(runner.state().objects[&forest]
        .abilities
        .iter()
        .any(|a| matches!(&*a.effect, Effect::GainLife { .. })));
    let index = choose_ability_index(&runner, terraformer, is_basic_land_type_choice)
        .expect("Terraformer's basic-land-type choice");
    let outcome = runner
        .activate(terraformer, index)
        .choose_option("Island")
        .resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    relayer(&mut runner);
    assert_eq!(subtypes(&runner, forest), ["Island"]);
    assert!(has_mana_ability_for(&runner, forest, ManaColor::Blue));
    assert!(!has_mana_ability_for(&runner, forest, ManaColor::Green));
    assert!(!runner.state().objects[&forest]
        .abilities
        .iter()
        .any(|a| matches!(&*a.effect, Effect::GainLife { .. })));
    assert_eq!(subtypes(&runner, opposing_forest), ["Forest"]);
    assert!(has_mana_ability_for(
        &runner,
        opposing_forest,
        ManaColor::Green
    ));
    let mana_index = runner.state().objects[&forest]
        .abilities
        .iter()
        .position(|a| matches!(&*a.effect, Effect::Mana { .. }))
        .expect("intrinsic mana ability");
    runner.activate(forest, mana_index).resolve();
    assert!(runner.state().objects[&forest].tapped);
    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(pool.total(), 1);
    assert_eq!(pool.count_color(ManaType::Blue), 1);
}

/// CR 113.7a + CR 608.2d: Flask's sacrifice is paid before its resolution
/// choice, and the chosen type still reaches the continuation without a source.
#[test]
fn elsewhere_flask_choice_continues_after_sacrifice() {
    parse_supported(ELSEWHERE_FLASK, "Elsewhere Flask", "Artifact");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flask = scenario
        .add_artifact_from_oracle(P0, "Elsewhere Flask", ELSEWHERE_FLASK)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let opposing_forest = scenario.add_basic_land(P1, ManaColor::Green);
    let mut runner = scenario.build();
    relayer(&mut runner);
    let index = choose_ability_index(&runner, flask, is_basic_land_type_choice)
        .expect("Flask's basic-land-type choice");
    let paused = runner.activate(flask, index).pay_with(&[flask]).resolve();
    paused.assert_zone(&[flask], Zone::Graveyard);
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = paused.final_waiting_for()
    else {
        panic!(
            "expected land choice after sacrifice: {:?}",
            paused.final_waiting_for()
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*choice_type, ChoiceType::BasicLandType);
    // CR 305.6: exactly the five basic land types are offered.
    let mut options = options.clone();
    options.sort();
    assert_eq!(options, ["Forest", "Island", "Mountain", "Plains", "Swamp"]);
    assert_eq!(subtypes(&runner, forest), ["Forest"]);
    assert_eq!(subtypes(&runner, mountain), ["Mountain"]);
    runner
        .act(GameAction::ChooseOption {
            choice: "Island".into(),
        })
        .expect("legal Island choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    relayer(&mut runner);
    assert_eq!(runner.state().objects[&flask].zone, Zone::Graveyard);
    for land in [forest, mountain] {
        assert_eq!(subtypes(&runner, land), ["Island"]);
        assert!(has_mana_ability_for(&runner, land, ManaColor::Blue));
        assert!(!has_mana_ability_for(&runner, land, ManaColor::Green));
        assert!(!has_mana_ability_for(&runner, land, ManaColor::Red));
    }
    assert_eq!(subtypes(&runner, opposing_forest), ["Forest"]);
}

/// R1 — CR 205.1a + CR 608.2h. Mistform Stalker becomes ONLY the chosen
/// creature type, and a second activation in the same turn binds its own
/// answer.
#[test]
fn mistform_stalker_becomes_only_the_chosen_creature_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stalker = scenario
        .add_creature_from_oracle(P0, "Mistform Stalker", 1, 1, MISTFORM_STALKER)
        .with_subtypes(vec!["Illusion"])
        .id();
    // Exactly the two activations' `{1}` (auto-tap is not modeled).
    scenario.with_mana_pool(P0, blue_mana(2));
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    relayer(&mut runner);

    // POSITIVE REACH-GUARD: the printed Illusion is there before activation,
    // so "not Illusion" below is not vacuous.
    assert!(
        has_subtype(&runner, stalker, "Illusion"),
        "printed Illusion: {:?}",
        subtypes(&runner, stalker)
    );
    let index = choose_ability_index(&runner, stalker, is_creature_type_choice)
        .expect("Mistform Stalker must carry its creature-type choice ability");
    let tce_before = runner.state().transient_continuous_effects.len();

    // ACTIVATION 1 — Elf.
    let first = runner
        .activate(stalker, index)
        .choose_option("Elf")
        .resolve();
    assert!(
        matches!(first.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 1 must resolve, got {:?}",
        first.final_waiting_for()
    );
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        tce_before + 1,
        "activation 1 must create one transient continuous effect"
    );
    relayer(&mut runner);
    assert!(
        has_subtype(&runner, stalker, "Elf"),
        "CR 608.2h: activation 1's Elf must apply: {:?}",
        subtypes(&runner, stalker)
    );
    assert!(
        !has_subtype(&runner, stalker, "Illusion"),
        "CR 205.1a: the chosen type REPLACES the creature types: {:?}",
        subtypes(&runner, stalker)
    );

    // ACTIVATION 2 — Goblin, same turn.
    let second = runner
        .activate(stalker, index)
        .choose_option("Goblin")
        .resolve();
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 2 must resolve, got {:?}",
        second.final_waiting_for()
    );
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        tce_before + 2,
        "activation 2 must create a second transient continuous effect"
    );
    relayer(&mut runner);
    assert!(
        has_subtype(&runner, stalker, "Goblin"),
        "activation 2 binds its own answer, Goblin: {:?}",
        subtypes(&runner, stalker)
    );
    assert!(
        !has_subtype(&runner, stalker, "Elf"),
        "CR 205.1a + CR 613.7: the later set effect replaces Elf: {:?}",
        subtypes(&runner, stalker)
    );
    assert!(
        !has_subtype(&runner, stalker, "Illusion"),
        "Illusion stays gone: {:?}",
        subtypes(&runner, stalker)
    );
}

/// R2 — CR 205.1a. Mistform Wall stops being a Wall, so its "has defender as
/// long as it's a Wall" static stops applying.
#[test]
fn mistform_wall_loses_defender_after_becoming_an_elf() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario
        .add_creature_from_oracle(P0, "Mistform Wall", 1, 4, MISTFORM_WALL)
        .with_subtypes(vec!["Illusion", "Wall"])
        .id();
    scenario.with_mana_pool(P0, blue_mana(1));
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    relayer(&mut runner);

    // POSITIVE CONTROL: as a Wall it has defender.
    assert!(
        runner.state().objects[&wall]
            .keywords
            .contains(&Keyword::Defender),
        "a Wall must have defender before activation: {:?}",
        runner.state().objects[&wall].keywords
    );
    let index = choose_ability_index(&runner, wall, is_creature_type_choice)
        .expect("Mistform Wall must carry its creature-type choice ability");

    let outcome = runner.activate(wall, index).choose_option("Elf").resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the activation must resolve, got {:?}",
        outcome.final_waiting_for()
    );
    relayer(&mut runner);

    assert!(
        has_subtype(&runner, wall, "Elf"),
        "the chosen Elf must apply: {:?}",
        subtypes(&runner, wall)
    );
    assert!(
        !has_subtype(&runner, wall, "Wall"),
        "CR 205.1a: Wall is replaced: {:?}",
        subtypes(&runner, wall)
    );
    assert!(
        !runner.state().objects[&wall]
            .keywords
            .contains(&Keyword::Defender),
        "no longer a Wall, so no defender: {:?}",
        runner.state().objects[&wall].keywords
    );
}

/// R3 — CR 205.1b + CR 608.2h. The granted "in addition to its other types"
/// ability adds each activation's own type and keeps every other one.
#[test]
fn mistform_sliver_two_activations_keep_both_chosen_types() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sliver = scenario
        .add_creature_from_oracle(P0, "Mistform Sliver", 1, 1, MISTFORM_SLIVER)
        .with_subtypes(vec!["Illusion", "Sliver"])
        .id();
    scenario.with_mana_pool(P0, blue_mana(2));
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    relayer(&mut runner);

    // REACH-GUARD: the Sliver grants itself the ability (layer 6).
    let index = choose_ability_index(&runner, sliver, is_creature_type_choice)
        .expect("Mistform Sliver must carry the granted creature-type choice ability");

    let first = runner
        .activate(sliver, index)
        .choose_option("Elf")
        .resolve();
    assert!(
        matches!(first.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 1 must resolve, got {:?}",
        first.final_waiting_for()
    );
    relayer(&mut runner);
    assert!(
        has_subtype(&runner, sliver, "Elf"),
        "activation 1's Elf must apply: {:?}",
        subtypes(&runner, sliver)
    );

    let index = choose_ability_index(&runner, sliver, is_creature_type_choice)
        .expect("the granted ability survives activation 1");
    let second = runner
        .activate(sliver, index)
        .choose_option("Goblin")
        .resolve();
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 2 must resolve, got {:?}",
        second.final_waiting_for()
    );
    relayer(&mut runner);

    for expected in ["Illusion", "Sliver", "Elf", "Goblin"] {
        assert!(
            has_subtype(&runner, sliver, expected),
            "CR 205.1b: {expected} must be kept/added: {:?}",
            subtypes(&runner, sliver)
        );
    }
}

/// R4 — CR 608.2h. Two live effects from ONE source on two different
/// recipients each keep their own chosen type.
#[test]
fn unnatural_selection_two_targets_keep_their_own_types() {
    // The consumer is tested with legal choices only; the independent
    // "other than Wall" chooser-exclusion gap is outside this regression.
    parse_supported(UNNATURAL_SELECTION, "Unnatural Selection", "Enchantment");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selection = scenario
        .add_enchantment_from_oracle(P0, "Unnatural Selection", UNNATURAL_SELECTION)
        .id();
    let bear_a = scenario
        .add_creature(P0, "Bear A", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear_b = scenario
        .add_creature(P0, "Bear B", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    scenario.with_mana_pool(P0, blue_mana(2));
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    relayer(&mut runner);

    assert!(has_subtype(&runner, bear_a, "Bear"));
    assert!(has_subtype(&runner, bear_b, "Bear"));
    let index = choose_ability_index(&runner, selection, is_creature_type_choice)
        .expect("Unnatural Selection must carry its creature-type choice ability");

    let first = runner
        .activate(selection, index)
        .target_object(bear_a)
        .choose_option("Elf")
        .resolve();
    assert!(
        matches!(first.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 1 must resolve, got {:?}",
        first.final_waiting_for()
    );
    let second = runner
        .activate(selection, index)
        .target_object(bear_b)
        .choose_option("Goblin")
        .resolve();
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 2 must resolve, got {:?}",
        second.final_waiting_for()
    );
    relayer(&mut runner);

    // POSITIVE rows double as reach-guards for the negatives.
    assert!(
        has_subtype(&runner, bear_a, "Elf"),
        "bear A must be an Elf: {:?}",
        subtypes(&runner, bear_a)
    );
    assert!(
        has_subtype(&runner, bear_b, "Goblin"),
        "bear B must be a Goblin: {:?}",
        subtypes(&runner, bear_b)
    );
    // CR 205.1a: each bare "becomes that type" replaces creature subtypes.
    assert!(!has_subtype(&runner, bear_a, "Bear"));
    assert!(!has_subtype(&runner, bear_b, "Bear"));
    assert!(
        !has_subtype(&runner, bear_a, "Goblin"),
        "CR 608.2h: bear A must not read activation 2's answer: {:?}",
        subtypes(&runner, bear_a)
    );
    assert!(
        !has_subtype(&runner, bear_b, "Elf"),
        "bear B must not read activation 1's answer: {:?}",
        subtypes(&runner, bear_b)
    );
}

/// R5 — CR 305.7. Jinx sets the target land's type: it loses Forest and its
/// green mana ability and gains Island and a blue one (CR 305.6).
#[test]
fn jinx_sets_target_land_to_only_the_chosen_basic_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_basic_land(P1, ManaColor::Green);
    let jinx = scenario
        .add_spell_to_hand_from_oracle(P0, "Jinx", true, JINX)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    relayer(&mut runner);

    // POSITIVE REACH-GUARD: a Forest with its green mana ability.
    assert!(
        has_subtype(&runner, forest, "Forest"),
        "{:?}",
        subtypes(&runner, forest)
    );
    assert!(has_mana_ability_for(&runner, forest, ManaColor::Green));

    let outcome = runner
        .cast(jinx)
        .target_object(forest)
        .choose_option("Island")
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Jinx must resolve, got {:?}",
        outcome.final_waiting_for()
    );
    relayer(&mut runner);

    assert!(
        has_subtype(&runner, forest, "Island"),
        "the chosen Island must apply: {:?}",
        subtypes(&runner, forest)
    );
    assert!(
        has_mana_ability_for(&runner, forest, ManaColor::Blue),
        "CR 305.6: an Island taps for blue"
    );
    assert!(
        !has_subtype(&runner, forest, "Forest"),
        "CR 305.7: the old land type is replaced: {:?}",
        subtypes(&runner, forest)
    );
    assert!(
        !has_mana_ability_for(&runner, forest, ManaColor::Green),
        "CR 305.7: the Forest mana ability is lost"
    );
}

/// R6 — CR 305.7 (last sentence). Navigator's Compass adds the chosen basic
/// land type and keeps the land's own type and mana ability.
#[test]
fn navigators_compass_adds_chosen_basic_type_in_addition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let compass = scenario
        .add_artifact_from_oracle(P0, "Navigator's Compass", NAVIGATORS_COMPASS)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    relayer(&mut runner);

    assert!(
        has_subtype(&runner, forest, "Forest"),
        "{:?}",
        subtypes(&runner, forest)
    );
    let index = choose_ability_index(&runner, compass, is_basic_land_type_choice)
        .expect("Navigator's Compass must carry its basic-land-type choice ability");

    let outcome = runner
        .activate(compass, index)
        .target_object(forest)
        .choose_option("Island")
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the activation must resolve, got {:?}",
        outcome.final_waiting_for()
    );
    relayer(&mut runner);

    assert!(
        has_subtype(&runner, forest, "Island"),
        "the chosen Island must be added: {:?}",
        subtypes(&runner, forest)
    );
    assert!(
        has_subtype(&runner, forest, "Forest"),
        "CR 305.7: the land keeps its own type: {:?}",
        subtypes(&runner, forest)
    );
    assert!(has_mana_ability_for(&runner, forest, ManaColor::Blue));
    assert!(has_mana_ability_for(&runner, forest, ManaColor::Green));
}
