//! Recipient-bound state durations start once and end once (CR 611.2b).
//! Rootwater uses its verbatim Oracle text through the activation pipeline;
//! Aura changes, source removal, untapping and phasing use real spell casts.

use engine::game::effects::attach::attach_to;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::{PhaseOutCause, PhaseStatus};
use engine::game::layers::flush_layers;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, AttachmentKind, Duration, Effect, EffectKind, FilterProp, SourceExclusion,
    StaticCondition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{LayersDirty, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use engine::types::keywords::{EchoCost, Keyword};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ROOTWATER: &str =
    "{T}: Gain control of target creature for as long as that creature is enchanted.";
const HOLY_STRENGTH: &str = "Enchant creature\nEnchanted creature gets +1/+2.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const BURST_OF_ENERGY: &str = "Untap target permanent.";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";
const FLEDGLING_OSPREY: &str = "This creature has flying as long as it's enchanted.";
const CLEVER_CONCEALMENT: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nAny number of target nonland permanents you control phase out. (Treat them and anything attached to them as though they don't exist until your next turn.)";
const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters the battlefield under your control.)";
const CHROMATIC_LANTERN: &str =
    "Lands you control have \"{T}: Add one mana of any color.\"\n{T}: Add one mana of any color.";

fn board() -> (GameScenario, [ObjectId; 2], [ObjectId; 2]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Draw-step card");
        }
    }
    let sources = [
        scenario
            .add_creature_from_oracle(P0, "Rootwater Matriarch", 2, 3, ROOTWATER)
            .id(),
        scenario
            .add_creature_from_oracle(P0, "Rootwater Matriarch", 2, 3, ROOTWATER)
            .id(),
    ];
    let recipients = [
        scenario.add_creature(P1, "First recipient", 2, 2).id(),
        scenario.add_creature(P1, "Second recipient", 2, 2).id(),
    ];
    (scenario, sources, recipients)
}

fn battlefield_aura(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(controller, "Holy Strength", HOLY_STRENGTH)
        .with_subtypes(vec!["Aura"])
        .id()
}

fn hand_aura(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Holy Strength", false, HOLY_STRENGTH)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], HOLY_STRENGTH)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn cast_aura(runner: &mut GameRunner, aura: ObjectId, recipient: ObjectId) -> CastOutcome {
    let object = &runner.state().objects[&aura];
    assert_eq!(object.zone, Zone::Hand);
    assert_eq!(object.card_types.core_types, vec![CoreType::Enchantment]);
    assert!(object
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Aura"));
    assert!(object
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Enchant(_))));

    let committed = runner.cast(aura).target_object(recipient).commit();
    assert_eq!(committed.state().objects[&aura].zone, Zone::Stack);
    let entry = committed
        .state()
        .stack
        .back()
        .expect("the Aura must be on the stack");
    assert_eq!(entry.source_id, aura);
    // CR 303.4a: the enchant target must be committed, not merely declared as test intent.
    assert_eq!(
        entry
            .ability()
            .expect("the Aura must carry its enchant target")
            .targets,
        vec![TargetRef::Object(recipient)]
    );

    let outcome = committed.resolve();
    outcome.assert_zone(&[aura], Zone::Battlefield);
    // CR 608.3c: the resolving Aura enters attached to its chosen target.
    assert_eq!(
        outcome.state().objects[&aura]
            .attached_to
            .and_then(|host| host.as_object()),
        Some(recipient)
    );
    assert!(outcome.state().objects[&recipient]
        .attachments
        .contains(&aura));
    outcome
}

fn instant(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn concealment(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Clever Concealment", true, CLEVER_CONCEALMENT)
        .from_oracle_text_with_keywords(&["Convoke"], CLEVER_CONCEALMENT)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn attach_setup(runner: &mut GameRunner, attachment: ObjectId, recipient: ObjectId) {
    attach_to(runner.state_mut(), attachment, recipient);
    assert_eq!(
        runner.state().objects[&attachment]
            .attached_to
            .and_then(|host| host.as_object()),
        Some(recipient),
        "the setup must actually attach the Aura or Equipment"
    );
    flush_layers(runner.state_mut());
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("Rootwater's real activated ability must be available")
}

fn take_control(runner: &mut GameRunner, source: ObjectId, recipient: ObjectId) {
    let index = activated_index(runner, source);
    let outcome = runner
        .activate(source, index)
        .target_object(recipient)
        .resolve();
    assert!(outcome.state().stack.is_empty());
    assert!(
        outcome.state().objects[&source].tapped,
        "activation paid its tap cost"
    );
    // CR 611.2b + CR 613.1b: the positive reach guard is actual control.
    assert_eq!(outcome.state().objects[&recipient].controller, P0);
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::ControllerChanged { object_id, old_controller: P1, new_controller: P0 }
            if *object_id == recipient
    )));
}

fn rootwater_effects(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| effect.source_id == source)
        .count()
}

/// CR 608.2b reach guard: the control ability resolved rather than fizzling,
/// so the negative assertions that follow it are not vacuous.
fn assert_gain_control_resolved(events: &[GameEvent], source: ObjectId) {
    assert!(
        events.iter().any(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::GainControl, source_id, .. }
                if *source_id == source
        )),
        "a legal target resolves rather than fizzling"
    );
}

fn enchanted_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::default().properties(vec![FilterProp::HasAttachment {
            kind: AttachmentKind::Aura,
            controller: None,
            exclude_source: SourceExclusion::Include,
        }]),
    )
}

#[test]
fn commandeer_preserves_permanent_spell_control_on_battlefield_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let permanent = scenario
        .add_artifact_to_hand_from_oracle(P0, "Chromatic Lantern", CHROMATIC_LANTERN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(P1, "Commandeer", true, COMMANDEER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let mut original = runner.cast(permanent).commit();
    assert_eq!(original.state().objects[&permanent].zone, Zone::Stack);
    assert_eq!(original.state().objects[&permanent].controller, P0);
    let stack_incarnation =
        ObjectIncarnationRef::from_object(&original.state().objects[&permanent]);
    original.act(GameAction::PassPriority).unwrap();
    let mut response = original.cast(commandeer).target_object(permanent).commit();
    let mut control_changed = false;
    for _ in 0..12 {
        if response.state().objects[&commandeer].zone != Zone::Stack
            && matches!(response.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let result = match response.state().waiting_for {
            WaitingFor::Priority { .. } => response.act(GameAction::PassPriority).unwrap(),
            WaitingFor::OptionalEffectChoice { .. } => response
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap(),
            ref other => panic!("unexpected Commandeer prompt: {other:?}"),
        };
        control_changed |= result.events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ControllerChanged { object_id, old_controller: P0, new_controller: P1 }
                    if *object_id == permanent
            )
        });
    }
    assert_eq!(response.state().objects[&commandeer].zone, Zone::Graveyard);
    // CR 613.1b: the real spell must change control before the permanent resolves.
    assert!(control_changed, "Commandeer must reach its control effect");
    assert_eq!(response.state().objects[&permanent].zone, Zone::Stack);
    assert_eq!(response.state().objects[&permanent].controller, P1);

    let outcome = response.resolve();
    outcome.assert_zone(&[permanent], Zone::Battlefield);
    outcome.assert_zone(&[commandeer], Zone::Graveyard);
    let resolved = &outcome.state().objects[&permanent];
    // CR 400.7a: the control effect survives the spell's new permanent incarnation.
    assert_ne!(
        ObjectIncarnationRef::from_object(resolved),
        stack_incarnation
    );
    assert_eq!(resolved.controller, P1);
    // CR 110.2b: the original caster remains the controller by default.
    assert_eq!(resolved.base_controller, Some(P0));
}

#[test]
fn rootwater_exact_oracle_shape_uses_recipient_enchanted_duration() {
    let parsed = parse_oracle_text(
        ROOTWATER,
        "Rootwater Matriarch",
        &[],
        &["Creature".into()],
        &["Merfolk".into()],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let ability = &parsed.abilities[0];
    assert_eq!(
        *ability.effect,
        Effect::GainControl {
            target: TargetFilter::Typed(TypedFilter::creature())
        }
    );
    assert_eq!(
        ability.duration,
        Some(Duration::ForAsLongAs {
            condition: StaticCondition::RecipientMatchesFilter {
                filter: enchanted_filter()
            },
        })
    );
}

#[test]
fn rootwater_initial_false_does_not_install_emit_or_restart_when_enchanted() {
    let (mut scenario, sources, recipients) = board();
    let existing = battlefield_aura(&mut scenario, P1);
    let later = hand_aura(&mut scenario);
    let mut runner = scenario.build();
    attach_setup(&mut runner, existing, recipients[1]);
    // Echo makes the otherwise invisible control-change side effect observable.
    {
        let target = runner.state_mut().objects.get_mut(&recipients[0]).unwrap();
        target
            .keywords
            .push(Keyword::Echo(EchoCost::Mana(ManaCost::generic(1))));
        target.base_keywords = target.keywords.clone();
        target.echo_due = false;
    }
    let next_effect_id = runner.state().next_continuous_effect_id;
    let index = activated_index(&runner, sources[0]);
    let outcome = runner
        .activate(sources[0], index)
        .target_object(recipients[0])
        .resolve();
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().objects[&sources[0]].tapped);
    assert_gain_control_resolved(outcome.events(), sources[0]);
    // CR 611.2b: no effect starts, including its control-change side effects.
    assert_eq!(outcome.state().objects[&recipients[0]].controller, P1);
    assert!(!outcome.state().objects[&recipients[0]].echo_due);
    assert_eq!(outcome.state().next_continuous_effect_id, next_effect_id);
    assert!(!outcome.events().iter().any(|event| matches!(event,
        GameEvent::ControllerChanged { object_id, .. } if *object_id == recipients[0]
    )));
    assert_eq!(rootwater_effects(&runner, sources[0]), 0);

    cast_aura(&mut runner, later, recipients[0]).assert_zone(&[later], Zone::Battlefield);
    assert!(runner.state().objects[&recipients[0]]
        .attachments
        .contains(&later));
    assert_eq!(runner.state().objects[&recipients[0]].controller, P1);
    // CR 303.4e: an Aura controlled by the other player is sufficient.
    take_control(&mut runner, sources[1], recipients[1]);
    assert_eq!(runner.state().objects[&existing].controller, P1);
    let effect = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|effect| effect.source_id == sources[1])
        .unwrap();
    let recipient = ObjectIncarnationRef::from_object(&runner.state().objects[&recipients[1]]);
    assert_eq!(effect.affected_recipient, Some(recipient));
    assert_eq!(effect.duration_subject, Some(recipient));
}

#[test]
fn rootwater_last_aura_loss_is_per_recipient_and_irreversible() {
    let (mut scenario, sources, recipients) = board();
    let first = battlefield_aura(&mut scenario, P0);
    let second = battlefield_aura(&mut scenario, P1);
    let unrelated = battlefield_aura(&mut scenario, P1);
    let remove_first = instant(&mut scenario, "Disenchant", DISENCHANT);
    let remove_second = instant(&mut scenario, "Disenchant", DISENCHANT);
    let replacement = hand_aura(&mut scenario);
    let mut runner = scenario.build();
    attach_setup(&mut runner, first, recipients[0]);
    attach_setup(&mut runner, second, recipients[0]);
    attach_setup(&mut runner, unrelated, recipients[1]);
    take_control(&mut runner, sources[0], recipients[0]);
    take_control(&mut runner, sources[1], recipients[1]);
    runner
        .cast(remove_first)
        .target_object(first)
        .resolve()
        .assert_zone(&[first], Zone::Graveyard);
    // CR 303.4b: one Aura remaining on this recipient sustains its duration.
    assert_eq!(runner.state().objects[&recipients[0]].controller, P0);
    assert_eq!(rootwater_effects(&runner, sources[0]), 1);
    runner
        .cast(remove_second)
        .target_object(second)
        .resolve()
        .assert_zone(&[second], Zone::Graveyard);
    // CR 611.2b: another recipient's Aura cannot sustain the ended member.
    assert_eq!(runner.state().objects[&recipients[0]].controller, P1);
    assert_eq!(rootwater_effects(&runner, sources[0]), 0);
    assert_eq!(runner.state().objects[&recipients[1]].controller, P0);
    assert_eq!(rootwater_effects(&runner, sources[1]), 1);
    cast_aura(&mut runner, replacement, recipients[0])
        .assert_zone(&[replacement], Zone::Battlefield);
    assert!(runner.state().objects[&recipients[0]]
        .attachments
        .contains(&replacement));
    assert_eq!(runner.state().objects[&recipients[0]].controller, P1);
    assert_eq!(rootwater_effects(&runner, sources[0]), 0);
}

#[test]
fn rootwater_equipment_alone_is_false_but_an_aura_is_true() {
    let (mut scenario, sources, recipients) = board();
    let equipment = scenario
        .add_artifact_from_oracle(P1, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let aura = battlefield_aura(&mut scenario, P1);
    let mut runner = scenario.build();
    attach_setup(&mut runner, equipment, recipients[0]);
    attach_setup(&mut runner, aura, recipients[1]);
    let index = activated_index(&runner, sources[0]);
    let outcome = runner
        .activate(sources[0], index)
        .target_object(recipients[0])
        .resolve();
    assert!(outcome.state().objects[&sources[0]].tapped);
    assert!(outcome.state().stack.is_empty());
    assert_gain_control_resolved(outcome.events(), sources[0]);
    // CR 303.4b: Equipment does not make its host enchanted.
    assert_eq!(outcome.state().objects[&recipients[0]].controller, P1);
    assert_eq!(rootwater_effects(&runner, sources[0]), 0);
    take_control(&mut runner, sources[1], recipients[1]);
}

#[test]
fn rootwater_source_untap_and_departure_do_not_end_recipient_duration() {
    let (mut scenario, sources, recipients) = board();
    let aura = battlefield_aura(&mut scenario, P1);
    let untap = instant(&mut scenario, "Burst of Energy", BURST_OF_ENERGY);
    let bounce = instant(&mut scenario, "Unsummon", UNSUMMON);
    let mut runner = scenario.build();
    attach_setup(&mut runner, aura, recipients[0]);
    take_control(&mut runner, sources[0], recipients[0]);
    runner.cast(untap).target_object(sources[0]).resolve();
    assert!(!runner.state().objects[&sources[0]].tapped);
    assert_eq!(runner.state().objects[&recipients[0]].controller, P0);
    runner
        .cast(bounce)
        .target_object(sources[0])
        .resolve()
        .assert_zone(&[sources[0]], Zone::Hand);
    // CR 113.7a + CR 611.2b: this duration tracks the recipient, not its source.
    assert_eq!(runner.state().objects[&recipients[0]].controller, P0);
    assert_eq!(rootwater_effects(&runner, sources[0]), 1);
}

#[test]
fn rootwater_source_can_depart_before_the_activation_resolves() {
    let (mut scenario, sources, recipients) = board();
    let aura = battlefield_aura(&mut scenario, P1);
    let bounce = instant(&mut scenario, "Unsummon", UNSUMMON);
    let mut runner = scenario.build();
    attach_setup(&mut runner, aura, recipients[0]);
    let index = activated_index(&runner, sources[0]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: sources[0],
            ability_index: index,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(recipients[0])),
        })
        .unwrap();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the paid activation must be pending"
    );
    assert!(runner.state().objects[&sources[0]].tapped);
    // CR 113.7a: Unsummon resolves above the pending activated ability.
    runner
        .cast(bounce)
        .target_object(sources[0])
        .resolve()
        .assert_zone(&[sources[0]], Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&recipients[0]].controller, P0);
    assert_eq!(rootwater_effects(&runner, sources[0]), 1);
}

fn advance_until_aura_phases_in(runner: &mut GameRunner, aura: ObjectId) {
    for _ in 0..96 {
        if runner.state().objects[&aura].is_phased_in() {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).unwrap();
            }
            other => panic!("unexpected prompt while reaching the next untap: {other:?}"),
        }
    }
    panic!("the Aura did not phase in at its controller's next untap");
}

#[test]
fn rootwater_separately_phased_aura_ends_only_the_last_aura_duration() {
    for has_second_aura in [false, true] {
        let (mut scenario, sources, recipients) = board();
        let aura = battlefield_aura(&mut scenario, P0);
        let second = has_second_aura.then(|| battlefield_aura(&mut scenario, P1));
        let phase_out = concealment(&mut scenario);
        let mut runner = scenario.build();
        attach_setup(&mut runner, aura, recipients[0]);
        if let Some(second) = second {
            attach_setup(&mut runner, second, recipients[0]);
        }
        take_control(&mut runner, sources[0], recipients[0]);
        let outcome = runner.cast(phase_out).target_object(aura).resolve();
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().objects[&aura].is_phased_out());
        assert!(outcome.state().objects[&recipients[0]].is_phased_in());
        assert!(outcome.events().iter().any(|event| matches!(event,
            GameEvent::PermanentPhasedOut { object_id, indirect: false } if *object_id == aura
        )));
        // CR 702.26b + CR 303.4b: retained links do not make a phased-out
        // Aura exist. Another phased-in Aura on this same host still counts.
        assert_eq!(
            matches_target_filter(
                runner.state(),
                recipients[0],
                &enchanted_filter(),
                &FilterContext::from_source(runner.state(), sources[0])
            ),
            has_second_aura
        );
        let expected_controller = if has_second_aura { P0 } else { P1 };
        assert_eq!(
            runner.state().objects[&recipients[0]].controller,
            expected_controller
        );
        assert_eq!(
            rootwater_effects(&runner, sources[0]),
            usize::from(has_second_aura)
        );
        advance_until_aura_phases_in(&mut runner, aura);
        assert!(runner.state().objects[&aura].is_phased_in());
        // CR 611.2b: phasing the Aura back in cannot recreate an ended effect.
        assert_eq!(
            runner.state().objects[&recipients[0]].controller,
            expected_controller
        );
        assert_eq!(
            rootwater_effects(&runner, sources[0]),
            usize::from(has_second_aura)
        );
    }
}

#[test]
fn rootwater_initially_phased_out_aura_cannot_start_control() {
    let (mut scenario, sources, recipients) = board();
    let aura = battlefield_aura(&mut scenario, P0);
    let live_aura = battlefield_aura(&mut scenario, P1);
    let phase_out = concealment(&mut scenario);
    let mut runner = scenario.build();
    attach_setup(&mut runner, aura, recipients[0]);
    attach_setup(&mut runner, live_aura, recipients[1]);
    runner.cast(phase_out).target_object(aura).resolve();
    assert!(runner.state().objects[&aura].is_phased_out());
    assert!(runner.state().objects[&recipients[0]].is_phased_in());
    let index = activated_index(&runner, sources[0]);
    let outcome = runner
        .activate(sources[0], index)
        .target_object(recipients[0])
        .resolve();
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().objects[&sources[0]].tapped);
    assert_gain_control_resolved(outcome.events(), sources[0]);
    assert_eq!(outcome.state().objects[&recipients[0]].controller, P1);
    assert_eq!(rootwater_effects(&runner, sources[0]), 0);
    take_control(&mut runner, sources[1], recipients[1]);
}

#[test]
fn printed_conditional_static_can_resume_after_reenchantment() {
    let (mut scenario, _, _) = board();
    let osprey = scenario
        .add_creature_from_oracle(P0, "Fledgling Osprey", 1, 1, FLEDGLING_OSPREY)
        .id();
    let first = hand_aura(&mut scenario);
    let replacement = hand_aura(&mut scenario);
    let removal = instant(&mut scenario, "Disenchant", DISENCHANT);
    let mut runner = scenario.build();
    cast_aura(&mut runner, first, osprey);
    assert!(runner.state().objects[&osprey].has_keyword(&Keyword::Flying));
    runner
        .cast(removal)
        .target_object(first)
        .resolve()
        .assert_zone(&[first], Zone::Graveyard);
    assert!(!runner.state().objects[&osprey].has_keyword(&Keyword::Flying));
    cast_aura(&mut runner, replacement, osprey);
    // CR 611.3a: printed conditional statics are live predicates, not latched durations.
    assert!(runner.state().objects[&osprey].has_keyword(&Keyword::Flying));
}

#[test]
fn rootwater_expiry_reaches_full_clean_and_incremental_action_boundaries() {
    for boundary in ["full", "clean", "incremental"] {
        let (mut scenario, sources, recipients) = board();
        let aura = battlefield_aura(&mut scenario, P0);
        let mut runner = scenario.build();
        attach_setup(&mut runner, aura, recipients[0]);
        take_control(&mut runner, sources[0], recipients[0]);

        // Prepare the same raw phase-status transition for each dirty lattice
        // arm. The spell-based tests above separately prove production phasing.
        runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .unwrap()
            .phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        runner.state_mut().layers_dirty = match boundary {
            "full" => LayersDirty::Full,
            "clean" => LayersDirty::Clean,
            "incremental" => {
                let entrant = create_object(
                    runner.state_mut(),
                    CardId(9000),
                    P0,
                    "Incremental entrant".into(),
                    Zone::Battlefield,
                );
                LayersDirty::EnteredObjects([entrant].into_iter().collect())
            }
            _ => unreachable!(),
        };
        runner.act(GameAction::PassPriority).unwrap();
        // CR 611.2b: every production flush boundary publishes the returned
        // controller, never a dead control effect's intermediate candidate.
        assert_eq!(
            runner.state().objects[&recipients[0]].controller,
            P1,
            "{boundary}"
        );
        assert_eq!(rootwater_effects(&runner, sources[0]), 0, "{boundary}");
        assert_eq!(runner.state().layers_dirty, LayersDirty::Clean);
    }
}

const THRULL_CHAMPION: &str =
    "Thrull creatures get +1/+1.\n{T}: Gain control of target Thrull for as long as you control this creature.";
const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";

/// CR 611.2b: "for as long as you control this creature" that is already over
/// when the ability resolves never starts, so the effect does nothing — even
/// when the effect would itself hand its controller the source (the Champion
/// targeting itself), which would otherwise make the duration read true.
#[test]
fn never_started_control_duration_cannot_sustain_itself() {
    for target_self in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let champion = scenario
            .add_creature_from_oracle(P1, "Thrull Champion", 2, 2, THRULL_CHAMPION)
            .with_subtypes(vec!["Thrull"])
            .id();
        let other_thrull = scenario
            .add_creature(P1, "Thrull", 1, 1)
            .with_subtypes(vec!["Thrull"])
            .id();
        let control_magic = scenario
            .add_enchantment_from_oracle(P0, "Control Magic", CONTROL_MAGIC)
            .with_subtypes(vec!["Aura"])
            .id();
        let removal = instant(&mut scenario, "Disenchant", DISENCHANT);
        let mut runner = scenario.build();
        attach_setup(&mut runner, control_magic, champion);
        assert_eq!(runner.state().objects[&champion].controller, P0);
        // CR 302.6: the fixture models control since P0's turn began.
        runner
            .state_mut()
            .objects
            .get_mut(&champion)
            .unwrap()
            .summoning_sick = false;

        let target = if target_self { champion } else { other_thrull };
        let index = activated_index(&runner, champion);
        runner
            .act(GameAction::ActivateAbility {
                source_id: champion,
                ability_index: index,
            })
            .unwrap();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1, "the activation is pending");
        assert!(runner.state().objects[&champion].tapped);

        // CR 113.7a: Disenchant resolves first; Control Magic's control ends,
        // so P0 no longer controls the Champion when its ability resolves.
        let outcome = runner.cast(removal).target_object(control_magic).resolve();
        outcome.assert_zone(&[control_magic], Zone::Graveyard);
        assert!(outcome.state().stack.is_empty());
        assert_gain_control_resolved(outcome.events(), champion);
        assert_eq!(
            runner.state().objects[&champion].controller,
            P1,
            "target_self={target_self}: the effect must not hand P0 the Champion"
        );
        assert_eq!(runner.state().objects[&other_thrull].controller, P1);
        assert_eq!(
            rootwater_effects(&runner, champion),
            0,
            "target_self={target_self}: nothing is installed"
        );
        assert!(
            !outcome.events().iter().any(|event| matches!(event,
                GameEvent::ControllerChanged { object_id, new_controller: P0, .. }
                    if *object_id == target)),
            "target_self={target_self}: a never-started duration emits no control change"
        );
    }
}
