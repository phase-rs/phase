//! "The same is true" type-changing static abilities.
//!
//! These tests exercise parser → static definition → layer application. The
//! stack cases distinguish a controlled spell from an owned card and prove that
//! a non-token object copy is excluded from the card arm (CR 108.2b).

use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ChosenSubtypeKind, ContinuousModification, ControllerRef, Duration, FilterProp,
    StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
};
use engine::types::card_type::{CoreType, SubtypeSet};
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const MASKWOOD_NEXUS: &str = "Creatures you control are every creature type. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const ARCANE_ADAPTATION: &str = "Creatures you control are the chosen type in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const LEYLINE_OF_TRANSFORMATION: &str = "Creatures you control are the chosen type in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const CONSPIRACY: &str = "Creatures you control are the chosen type. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const RUKARUMEL: &str = "Slivers you control and nontoken creatures you control are the chosen type in addition to their other creature types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const ROSHAN: &str = "Other creatures you control are Assassins in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const BIOTRANSFERENCE: &str = "Creatures you control are artifacts in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.";
const BIOTRANSFERENCE_FULL: &str = "Creatures you control are artifacts in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.\nWhenever you cast an artifact spell, you lose 1 life and create a 2/2 black Necron Warrior artifact creature token.";
const ENCROACHING_MYCOSYNTH: &str = "Nonland permanents you control are artifacts in addition to their other types. The same is true for permanent spells you control and nonland permanent cards you own that aren't on the battlefield.";
const GOBLIN_MASTERMIND: &str = "As long as you control The Goblin Mastermind or it's your commander, permanents you control are Kindred Goblins in addition to their other types. The same is true for spells you control and cards that you own that aren't on the battlefield.";

fn parsed_static(oracle: &str, name: &str) -> engine::types::ability::StaticDefinition {
    let parsed = parse_oracle_text(oracle, name, &[], &[], &[]);
    let debug = format!("{parsed:#?}");
    assert!(
        !debug.contains("Unimplemented"),
        "{name} must have no residual continuation gap:\n{debug}"
    );
    assert_eq!(
        parsed.statics.len(),
        1,
        "{name} must lower its complete continuation into one static definition"
    );
    parsed.statics.into_iter().next().unwrap()
}

fn typed(filter: &TargetFilter) -> &engine::types::ability::TypedFilter {
    let TargetFilter::Typed(filter) = filter else {
        panic!("expected a typed continuation arm, got {filter:?}")
    };
    filter
}

fn assert_battlefield_filter(filter: &TargetFilter) {
    match filter {
        TargetFilter::Typed(filter) => assert!(filter.properties.iter().any(|property| {
            matches!(
                property,
                FilterProp::InZone {
                    zone: Zone::Battlefield
                }
            )
        })),
        TargetFilter::Or { filters } => {
            assert!(
                !filters.is_empty(),
                "compound antecedent must retain its arms"
            );
            for filter in filters {
                assert_battlefield_filter(filter);
            }
        }
        other => panic!("unexpected battlefield antecedent: {other:?}"),
    }
}

fn assert_three_recipient_arms(definition: &engine::types::ability::StaticDefinition, name: &str) {
    let Some(TargetFilter::Or { filters }) = definition.affected.as_ref() else {
        panic!("{name}: expected three recipient arms, got {definition:#?}")
    };
    assert_eq!(filters.len(), 3, "battlefield, spell, and card arms");
    assert_battlefield_filter(&filters[0]);

    let spells = typed(&filters[1]);
    assert_eq!(spells.controller, Some(ControllerRef::You));
    assert!(spells
        .properties
        .iter()
        .any(|property| { matches!(property, FilterProp::InZone { zone: Zone::Stack }) }));

    let cards = typed(&filters[2]);
    assert!(cards.properties.iter().any(|property| {
        matches!(
            property,
            FilterProp::Owned {
                controller: ControllerRef::You
            }
        )
    }));
    assert!(
        cards
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::RepresentedByCard)),
        "the card arm must exclude tokens and non-token copies"
    );
    assert!(cards.properties.iter().any(|property| {
        matches!(
            property,
            FilterProp::InAnyZone { zones }
                if zones == &vec![
                    Zone::Library,
                    Zone::Hand,
                    Zone::Graveyard,
                    Zone::Stack,
                    Zone::Exile,
                    Zone::Command,
                ]
        )
    }));
}

/// Every exact supported Oracle form lowers to one complete continuous static;
/// none retains the former `Unimplemented` continuation tail.
#[test]
fn all_supported_same_is_true_type_forms_parse_to_one_static() {
    for (name, oracle) in [
        ("Maskwood Nexus", MASKWOOD_NEXUS),
        ("Arcane Adaptation", ARCANE_ADAPTATION),
        ("Leyline of Transformation", LEYLINE_OF_TRANSFORMATION),
        ("Conspiracy", CONSPIRACY),
        ("Rukarumel, Biologist", RUKARUMEL),
        ("Roshan, Hidden Magister", ROSHAN),
        ("Biotransference", BIOTRANSFERENCE),
        ("Encroaching Mycosynth", ENCROACHING_MYCOSYNTH),
        ("The Goblin Mastermind", GOBLIN_MASTERMIND),
    ] {
        let definition = parsed_static(oracle, name);
        assert_three_recipient_arms(&definition, name);
    }
}

/// Conspiracy's chosen-type sentence replaces creature subtypes. Arcane
/// Adaptation and Leyline of Transformation use the additive modification.
#[test]
fn conspiracy_uses_replacing_chosen_creature_type_modifications() {
    let definition = parsed_static(CONSPIRACY, "Conspiracy");
    assert_eq!(
        definition.modifications,
        vec![
            ContinuousModification::RemoveAllSubtypes {
                set: SubtypeSet::Creature,
            },
            ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            },
        ]
    );
}

/// The Goblin Mastermind's optional gate is source-relative and has its two
/// semantic alternatives typed, rather than degrading to `Unrecognized`.
#[test]
fn goblin_mastermind_uses_explicit_source_or_commander_gate() {
    let definition = parsed_static(GOBLIN_MASTERMIND, "The Goblin Mastermind");
    let Some(StaticCondition::Or { conditions }) = definition.condition else {
        panic!("expected an explicit source/commander Or condition")
    };
    assert_eq!(conditions.len(), 2);
    let StaticCondition::SourceMatchesFilter {
        filter: TargetFilter::Typed(first),
    } = &conditions[0]
    else {
        panic!("first gate arm must match the source")
    };
    assert_eq!(first.controller, Some(ControllerRef::You));
    let StaticCondition::SourceMatchesFilter {
        filter: TargetFilter::Typed(second),
    } = &conditions[1]
    else {
        panic!("second gate arm must match the source")
    };
    assert_eq!(
        second.properties,
        vec![
            FilterProp::Owned {
                controller: ControllerRef::You,
            },
            FilterProp::IsCommander,
        ]
    );
}

/// The recognizer is all-consuming: a malformed continuation cannot silently
/// claim the battlefield sentence through an older prefix parser. It remains a
/// whole-line `Unimplemented` residual until the full grammar is supported.
#[test]
fn malformed_same_is_true_continuation_is_an_honest_whole_line_residual() {
    let oracle = "Creatures you control are artifacts in addition to their other types. The same is true for creature spells you control.";
    let parsed = parse_oracle_text(oracle, "Malformed Same Is True", &[], &[], &[]);
    assert!(
        parsed.statics.is_empty(),
        "an incomplete continuation must not retain a narrower static: {parsed:#?}"
    );
    assert!(
        parsed.abilities.iter().any(|ability| matches!(
            ability.effect.as_ref(),
            engine::types::ability::Effect::Unimplemented {
                description: Some(fragment),
                ..
            } if fragment.contains(oracle)
        )),
        "the complete malformed line must be retained as an Unimplemented residual: {parsed:#?}"
    );
}

/// The subject and modification axes vary independently across this grammar:
/// Rukarumel retains its two battlefield subject arms, Roshan preserves
/// `Other` plus Assassin, and Mycosynth retains nonland permanent scope.
#[test]
fn specialized_same_is_true_forms_preserve_scopes_and_modifications() {
    let rukarumel = parsed_static(RUKARUMEL, "Rukarumel, Biologist");
    assert_eq!(
        rukarumel.modifications,
        vec![ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType,
        }],
        "Rukarumel must add, rather than replace, the chosen creature type"
    );
    let Some(TargetFilter::Or {
        filters: rukarumel_arms,
    }) = rukarumel.affected.as_ref()
    else {
        panic!("Rukarumel must have the complete recipient union: {rukarumel:#?}")
    };
    let TargetFilter::Or {
        filters: battlefield_arms,
    } = &rukarumel_arms[0]
    else {
        panic!("Rukarumel must retain its compound battlefield antecedent")
    };
    assert_eq!(battlefield_arms.len(), 2);
    assert!(matches!(
        &battlefield_arms[0],
        TargetFilter::Typed(filter)
            if filter.controller == Some(ControllerRef::You)
                && filter.type_filters.contains(&TypeFilter::Subtype("Sliver".to_string()))
    ));
    assert!(matches!(
        &battlefield_arms[1],
        TargetFilter::Typed(filter)
            if filter.controller == Some(ControllerRef::You)
                && filter.type_filters.contains(&TypeFilter::Creature)
                && filter.properties.contains(&FilterProp::NonToken)
    ));

    let roshan = parsed_static(ROSHAN, "Roshan, Hidden Magister");
    assert_eq!(
        roshan.modifications,
        vec![ContinuousModification::AddSubtype {
            subtype: "Assassin".to_string(),
        }],
        "Roshan must add Assassin rather than replace existing types"
    );
    let Some(TargetFilter::Or {
        filters: roshan_arms,
    }) = roshan.affected.as_ref()
    else {
        panic!("Roshan must have the complete recipient union: {roshan:#?}")
    };
    assert!(matches!(
        &roshan_arms[0],
        TargetFilter::Typed(filter)
            if filter.controller == Some(ControllerRef::You)
                && filter.type_filters.contains(&TypeFilter::Creature)
                && filter.properties.contains(&FilterProp::Another)
    ));

    let mycosynth = parsed_static(ENCROACHING_MYCOSYNTH, "Encroaching Mycosynth");
    assert_eq!(
        mycosynth.modifications,
        vec![ContinuousModification::AddType {
            core_type: CoreType::Artifact,
        }],
        "Mycosynth must add Artifact in Layer 4"
    );
    let Some(TargetFilter::Or {
        filters: mycosynth_arms,
    }) = mycosynth.affected.as_ref()
    else {
        panic!("Mycosynth must have the complete recipient union: {mycosynth:#?}")
    };
    for filter in mycosynth_arms {
        let TargetFilter::Typed(filter) = filter else {
            panic!("Mycosynth recipient arm must be typed: {filter:?}")
        };
        assert!(filter.type_filters.contains(&TypeFilter::Permanent));
        assert!(filter
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
    }
}

fn is_artifact(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Artifact)
}

/// Install the canonical stack membership record for a test spell. A live
/// object's `Zone::Stack` field alone is intentionally insufficient: the Stack
/// is represented by `GameState::stack` entries (CR 405.1).
fn push_test_spell(
    state: &mut engine::types::game_state::GameState,
    id: ObjectId,
    controller: engine::types::player::PlayerId,
) {
    let card_id = state.objects[&id].card_id;
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
}

/// CR 108.2b + CR 109.2b + CR 400.1: Biotransference reaches each supported
/// card zone and both stack arms. This also proves off-battlefield controllers
/// survive a remote type reset.
#[test]
fn biotransference_applies_to_card_zones_and_distinguishes_stack_arms() {
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature_from_oracle(P0, "Biotransference", 1, 1, BIOTRANSFERENCE)
        .id();
    let battlefield = scenario.add_creature(P0, "Battlefield Test", 1, 1).id();
    let opponent_battlefield = scenario.add_creature(P1, "Opponent Test", 1, 1).id();
    let hand = scenario.add_creature_to_hand(P0, "Hand Test", 1, 1).id();
    let graveyard = scenario
        .add_creature_to_graveyard(P0, "Graveyard Test", 1, 1)
        .id();
    let library = scenario.add_creature_to_hand(P0, "Library Test", 1, 1).id();
    let exile = scenario.add_creature_to_hand(P0, "Exile Test", 1, 1).id();
    let command = scenario.add_creature_to_hand(P0, "Command Test", 1, 1).id();
    let owner_arm = scenario.add_creature_to_hand(P0, "Owner Arm", 1, 1).id();
    let control_arm = scenario.add_creature_to_hand(P1, "Control Arm", 1, 1).id();
    let copied_card = scenario
        .add_creature_to_hand(P0, "Copy Exclusion", 1, 1)
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, library, Zone::Library, &mut events);
        move_to_zone(state, exile, Zone::Exile, &mut events);
        move_to_zone(state, command, Zone::Command, &mut events);
        move_to_zone(state, owner_arm, Zone::Stack, &mut events);
        move_to_zone(state, control_arm, Zone::Stack, &mut events);
        move_to_zone(state, copied_card, Zone::Stack, &mut events);

        // Owner arm: card owned by P0 but cast/controlled by P1.
        state.objects.get_mut(&owner_arm).unwrap().controller = P1;
        // Controlled-spell arm: card owned by P1 but cast/controlled by P0.
        state.objects.get_mut(&control_arm).unwrap().controller = P0;
        // A non-token copy is neither a token nor a card (CR 108.2b). It is
        // deliberately not controlled by P0, so neither continuation arm fits.
        let copied = state.objects.get_mut(&copied_card).unwrap();
        copied.controller = P1;
        copied.is_copy = true;

        // `move_to_zone` deliberately does not synthesize a StackEntry: real
        // casts create one through the casting pipeline. Populate the canonical
        // Stack list so this fixture models three live spells (CR 405.1).
        push_test_spell(state, owner_arm, P1);
        push_test_spell(state, control_arm, P0);
        push_test_spell(state, copied_card, P1);

        state.layers_dirty.mark_full();
        evaluate_layers(state);
    }

    for id in [
        battlefield,
        hand,
        graveyard,
        library,
        exile,
        command,
        owner_arm,
        control_arm,
    ] {
        assert!(
            is_artifact(&runner, id),
            "{id:?} should gain Artifact; source={:#?}; object={:#?}",
            runner.state().objects[&source].static_definitions,
            runner.state().objects[&id]
        );
    }
    assert!(
        !is_artifact(&runner, opponent_battlefield),
        "opponent-controlled battlefield creature is outside the antecedent"
    );
    assert!(
        !is_artifact(&runner, copied_card),
        "a non-token object copy is not represented by a card"
    );
    assert_eq!(
        runner.state().objects[&owner_arm].controller,
        P1,
        "off-battlefield controller must survive the remote type reset"
    );

    // CR 400.1 + CR 611.3a + CR 613.1d: When the static source leaves the
    // battlefield, a forced full layer pass must reset every previously
    // affected remote-zone object's types and remove the now-inapplicable
    // type change.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
    flush_layers(runner.state_mut());
    for id in [
        battlefield,
        hand,
        graveyard,
        library,
        exile,
        command,
        owner_arm,
        control_arm,
    ] {
        assert!(
            !is_artifact(&runner, id),
            "{id:?} must reset when Biotransference leaves the battlefield"
        );
    }
}

/// CR 400.7g + CR 613.1d: A spell's cast-time static grant is independent of
/// a same-is-true type effect. Recomputing its remote Layer-4 characteristics
/// must not reseed its ability definitions from the printed-card baseline.
#[test]
fn remote_type_reset_preserves_cast_time_spell_grants() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Biotransference", 1, 1, BIOTRANSFERENCE)
        .id();
    let spell = scenario
        .add_creature_to_hand(P0, "Granted Spell", 1, 1)
        .id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, spell, Zone::Stack, &mut events);
        push_test_spell(state, spell, P0);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeCountered));
        state.layers_dirty.mark_full();
        evaluate_layers(state);
    }

    assert!(
        is_artifact(&runner, spell),
        "Biotransference must still modify the creature spell's type"
    );
    assert!(
        runner.state().objects[&spell]
            .static_definitions
            .iter_unchecked()
            .any(|definition| definition.mode == StaticMode::CantBeCountered),
        "the remote type pass must preserve the spell's cast-time grant"
    );
}

/// CR 613.1b + CR 611.3a + CR 613.1d: a Layer-2 theft changes the source's
/// controller before its controller-relative type static is evaluated. `You`
/// must rebind to the new controller for both battlefield and remote-zone arms.
#[test]
fn source_control_change_rebinds_same_is_true_you_scope() {
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature_from_oracle(P0, "Biotransference", 1, 1, BIOTRANSFERENCE)
        .id();
    let thief = scenario
        .add_creature(P1, "Control Effect Source", 1, 1)
        .id();
    let p0_hand = scenario.add_creature_to_hand(P0, "P0 Hand", 1, 1).id();
    let p1_hand = scenario.add_creature_to_hand(P1, "P1 Hand", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().add_transient_continuous_effect(
        thief,
        P1,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: source },
        vec![ContinuousModification::ChangeController],
        None,
    );
    evaluate_layers(runner.state_mut());

    assert_eq!(runner.state().objects[&source].controller, P1);
    assert!(
        is_artifact(&runner, p1_hand),
        "the stolen source's `you` must resolve as P1 for the owned-card arm"
    );
    assert!(
        !is_artifact(&runner, p0_hand),
        "the former controller's card must lose the source-relative effect"
    );
}

/// CR 205.3m + CR 611.3a + CR 613.1d: Maskwood Nexus's owned-card arm gives
/// a creature card in hand every creature type, while an opponent's hand card
/// remains outside the source-relative effect.
#[test]
fn maskwood_nexus_changes_owned_creature_cards_outside_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Maskwood Nexus", 1, 1, MASKWOOD_NEXUS)
        .id();
    let owned_card = scenario.add_creature_to_hand(P0, "Owned Test", 1, 1).id();
    let opponent_card = scenario
        .add_creature_to_hand(P1, "Opponent Test", 1, 1)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];
    evaluate_layers(runner.state_mut());

    for subtype in ["Elf", "Goblin"] {
        assert!(
            runner.state().objects[&owned_card]
                .card_types
                .subtypes
                .contains(&subtype.to_string()),
            "owned hand card must gain {subtype}"
        );
        assert!(
            !runner.state().objects[&opponent_card]
                .card_types
                .subtypes
                .contains(&subtype.to_string()),
            "opponent hand card must not gain {subtype}"
        );
    }
}

/// CR 601.2a + CR 603.2 + CR 613.1d: a creature spell becomes an Artifact on
/// the stack before Biotransference's cast trigger is collected, so the real
/// cast pipeline loses one life and creates the printed Necron Warrior token.
#[test]
fn biotransference_casts_a_creature_spell_as_an_artifact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Biotransference", 1, 1, BIOTRANSFERENCE_FULL)
        .id();
    let creature_spell = scenario
        .add_creature_to_hand(P0, "Artifact Trigger Test", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(creature_spell).resolve();

    outcome.assert_life_delta(P0, -1);
    assert!(
        is_artifact(&runner, creature_spell),
        "the creature spell must become an artifact while it is on the stack"
    );
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.zone == Zone::Battlefield)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "Biotransference must create exactly one token"
    );
    let token = tokens[0];
    assert_eq!(token.name, "Necron Warrior");
    assert_eq!(token.power, Some(2));
    assert_eq!(token.toughness, Some(2));
    assert_eq!(token.color, vec![ManaColor::Black]);
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    for subtype in ["Necron", "Warrior"] {
        assert!(
            token.card_types.subtypes.contains(&subtype.to_string()),
            "Biotransference's token must have the printed {subtype} subtype: {token:#?}"
        );
    }
}
