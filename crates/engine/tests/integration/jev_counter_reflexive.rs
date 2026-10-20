//! U12 mandatory adjustment and U13 exact actual-removal reflexive delivery.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CounterAdjustment, Effect, EffectKind, ObjectScope,
    QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition, StaticDefinition,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::events::GameEvent;
use engine::types::game_state::{CounterRemoveChoice, GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::ResolutionStateWire;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

// Synthetic class excerpts: the first sentence is Immard's exact printed parent.
const PARENT: &str = "Whenever Immard, the Stormcleaver enters or attacks, put a charge counter on it or remove one from it.";
const X_TEXT: &str = "Put X charge counters on target creature or remove X charge counters from it. You gain 1 life.";
const THREE_TEXT: &str = "Put three charge counters on target creature or remove three charge counters from it. You gain 1 life.";
const ZERO_TEXT: &str = "Put 0 charge counters on target creature or remove 0 charge counters from it. You gain 1 life.";
const CLOCK: &str = "Buyback {3} (You may pay an additional {3} as you cast this spell. If you do, put this card into your hand as it resolves.)\nChoose a counter on target permanent or suspended card. Remove that counter from that permanent or card or put another of those counters on it.";
const IMMARD: &str = "Whenever Immard, the Stormcleaver enters or attacks, put a charge counter on it or remove one from it. When you remove a counter this way, choose one —\n• Immard deals 4 damage to any target.\n• Immard gains lifelink and indestructible until end of turn.";
const P2: PlayerId = PlayerId(2);

struct U13Fixture {
    runner: GameRunner,
    source: ObjectId,
    blink: ObjectId,
    control: ObjectId,
    artifact: ObjectId,
    haste: ObjectId,
    live_grant: ObjectId,
    counter: ObjectId,
}

fn u13_until(r: &mut GameRunner, reached: impl Fn(&GameState) -> bool) {
    for _ in 0..90 {
        if reached(r.state()) {
            return;
        }
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            r.state().waiting_for
        );
        r.act(GameAction::PassPriority).unwrap();
    }
    panic!("public route did not reach its required boundary");
}

fn u13_response(
    r: &mut GameRunner,
    spell: ObjectId,
    source: ObjectId,
    recipient: Option<PlayerId>,
) {
    if let Some(player) = recipient {
        r.cast(spell)
            .target_player(player)
            .target_object(source)
            .commit();
    } else {
        r.cast(spell).target_object(source).commit();
    }
    u13_until(r, |state| state.objects[&spell].zone == Zone::Graveyard);
}

fn u13_fixture(
    text: &str,
    initial: u32,
    install: impl FnOnce(&mut GameScenario, ObjectId, &mut Vec<ReplacementDefinition>),
) -> U13Fixture {
    u13_fixture_with_responses(text, initial, install, |_, _| {})
}

fn u13_fixture_with_responses(
    text: &str,
    initial: u32,
    install: impl FnOnce(&mut GameScenario, ObjectId, &mut Vec<ReplacementDefinition>),
    responses: impl FnOnce(&mut GameRunner, ObjectId),
) -> U13Fixture {
    use engine::types::mana::ManaColor;
    let mut s = GameScenario::new_n_player(3, 13);
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(
        P0,
        [ManaType::Blue, ManaType::Red, ManaType::White]
            .into_iter()
            .flat_map(|kind| vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); 20])
            .collect(),
    );
    for player in [P0, P1, P2] {
        s.with_library_top(player, &["Safe draw", "Safe draw", "Safe draw"]);
    }
    let source = s
        .add_creature_to_hand_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, text)
        .as_legendary()
        .with_subtypes(vec!["Human", "Soldier"])
        .with_color(vec![ManaColor::Blue, ManaColor::Red, ManaColor::White])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Red,
                ManaCostShard::White,
            ],
        })
        .id();
    let stock = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic stock response",
            true,
            &format!("Put {initial} charge counters on target creature."),
        )
        .id();
    let blink = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic blink response",
            true,
            "Exile target creature. Return it to the battlefield under its owner's control.",
        )
        .id();
    let control = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic control response",
            true,
            "Target player gains control of target creature until end of turn.",
        )
        .id();
    let artifact = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic type response",
            true,
            "Target creature becomes an artifact in addition to its other types until end of turn.",
        )
        .id();
    let haste = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic haste response",
            true,
            "Target creature gains haste until end of turn.",
        )
        .id();
    let live_grant = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic live keyword response",
            true,
            "Target creature gains flying until end of turn.",
        )
        .id();
    let counter = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic ability counter",
            true,
            "Counter target activated or triggered ability.",
        )
        .id();
    let mut pending_definitions = Vec::new();
    install(&mut s, source, &mut pending_definitions);
    let mut runner = s.build();
    runner
        .state_mut()
        .pending_damage_replacements
        .extend(pending_definitions);
    runner.cast(source).commit();
    u13_until(&mut runner, |state| {
        state.objects[&source].zone == Zone::Battlefield
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::AbilityModeChoice { .. } | WaitingFor::TriggerTargetSelection { .. }
    ));
    if initial > 0 {
        u13_response(&mut runner, stock, source, None);
    }
    responses(&mut runner, source);
    u13_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
    });
    assert_eq!(n(&runner, source, &charge()), initial);
    U13Fixture {
        runner,
        source,
        blink,
        control,
        artifact,
        haste,
        live_grant,
        counter,
    }
}

fn u13_remove_and_announce(
    r: &mut GameRunner,
    source: ObjectId,
    mode: usize,
    target: PlayerId,
) -> ObjectId {
    let before = n(r, source, &charge());
    prompt(r, 2);
    let actions = r.act(GameAction::ChooseBranch { index: 0 }).unwrap();
    assert_eq!(event_count(&actions.events, source, &charge(), false), 1);
    assert_eq!(n(r, source, &charge()), before - 1);
    u13_until(r, |state| {
        matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
    });
    assert!(r
        .act(GameAction::SelectModes {
            indices: vec![usize::MAX]
        })
        .is_err());
    r.act(GameAction::SelectModes {
        indices: vec![mode],
    })
    .unwrap();
    if mode == 0 {
        let action = engine::ai_support::legal_actions(r.state()).into_iter().find(|action|
            matches!(action, GameAction::ChooseTarget { target: Some(engine::types::ability::TargetRef::Player(player)) } if *player == target))
            .expect("damage child must announce its own legal player target");
        r.act(action).unwrap();
    }
    let child = r
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .expect("independent child on stack");
    assert_eq!(child.controller, P0);
    let ability = child.ability().unwrap();
    assert!(ability
        .condition
        .as_ref()
        .is_none_or(|condition| !condition.has_when_you_do_marker()));
    assert!(ability.context.counter_removal_result.is_none());
    assert!(
        r.state().return_result_frames.is_empty(),
        "independent child no longer owns parent receipts"
    );
    child.id
}

#[test]
fn u13_public_full_immard_removal_damage_and_counter_independence() {
    // CR 603.12 + CR 603.3c: the removal precedes modes, target, and responses.
    for counter_child in [false, true] {
        let mut f = u13_fixture(IMMARD, 1, |_, _, _| {});
        let child = u13_remove_and_announce(&mut f.runner, f.source, 0, P2);
        if counter_child {
            f.runner.cast(f.counter).target_object(child).resolve();
        }
        finish(&mut f.runner, &mut Vec::new());
        assert_eq!(f.runner.life(P2), if counter_child { 20 } else { 16 });
        assert_eq!(
            n(&f.runner, f.source, &charge()),
            0,
            "countering the child does not refund removal"
        );
        assert!(f.runner.state().return_result_frames.is_empty());
    }
}

#[test]
fn u13_public_addition_empty_prevention_and_actual_removal_controls() {
    for (initial, add, prevent) in [
        (0, true, false),
        (2, true, false),
        (2, false, true),
        (2, false, false),
    ] {
        let mut f = u13_fixture(IMMARD, initial, |s, source, _| {
            if prevent {
                s.add_creature(P0, "Synthetic terminal removal prevention", 2, 2)
                    .with_replacement_definition(
                        ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                            .valid_card(TargetFilter::SpecificObject { id: source })
                            .quantity_modification(QuantityModification::Prevent),
                    );
            }
        });
        if !add && !prevent {
            u13_remove_and_announce(&mut f.runner, f.source, 0, P2);
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(f.runner.life(P2), 16);
        } else {
            prompt(&f.runner, if initial == 0 { 1 } else { 2 });
            assert!(f
                .runner
                .act(GameAction::ChooseBranch { index: usize::MAX })
                .is_err());
            let index = if add && initial > 0 { 1 } else { 0 };
            f.runner.act(GameAction::ChooseBranch { index }).unwrap();
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(f.runner.life(P2), 20);
            assert_eq!(n(&f.runner, f.source, &charge()), initial + u32::from(add));
        }
    }
}

fn u13_keyword_then_attack(f: &mut U13Fixture) -> ObjectId {
    use engine::types::keywords::Keyword;
    let original = ObjectIncarnationRef::from_object(&f.runner.state().objects[&f.source]);
    u13_remove_and_announce(&mut f.runner, f.source, 1, P2);
    finish(&mut f.runner, &mut Vec::new());
    for keyword in [Keyword::Lifelink, Keyword::Indestructible] {
        assert!(f.runner.state().objects[&f.source]
            .keywords
            .contains(&keyword));
    }
    u13_response(&mut f.runner, f.artifact, f.source, None);
    u13_response(&mut f.runner, f.haste, f.source, None);
    assert!(f.runner.state().objects[&f.source]
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Artifact));
    f.runner.advance_to_phase(Phase::DeclareAttackers);
    f.runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(f.source, engine::game::combat::AttackTarget::Player(P2))],
            bands: vec![],
        })
        .unwrap();
    u13_until(&mut f.runner, |state| {
        matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
    });
    assert!(original.is_current(f.runner.state()));
    u13_remove_and_announce(&mut f.runner, f.source, 0, P2)
}

fn u13_damage_record(f: &U13Fixture, old: ObjectIncarnationRef, damage: u32, returned: bool) {
    use engine::types::card_type::{CoreType, Supertype};
    use engine::types::keywords::Keyword;
    let record = f
        .runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .find(|record| {
            record.source_id == f.source && record.source_incarnation == Some(old.incarnation)
        })
        .unwrap();
    assert_eq!(record.amount, damage);
    assert_eq!(record.source_controller, P1);
    assert_eq!(record.source_controller_snapshot, P1);
    assert_eq!(record.source_owner, P0);
    assert_eq!(record.source_zone, Zone::Battlefield);
    assert_eq!(record.source_name, "Immard, the Stormcleaver");
    assert!(record.source_core_types.contains(&CoreType::Artifact));
    assert!(record.source_core_types.contains(&CoreType::Creature));
    assert_eq!(record.source_subtypes, ["Human", "Soldier"]);
    assert!(record.source_supertypes.contains(&Supertype::Legendary));
    assert_eq!(
        (record.source_power, record.source_toughness),
        (Some(4), Some(4))
    );
    assert_eq!(record.source_mana_value, 4);
    assert_eq!(record.source_colors.len(), 3);
    for keyword in [Keyword::Lifelink, Keyword::Indestructible, Keyword::Haste] {
        assert!(record.source_keywords.contains(&keyword));
    }
    assert_eq!(
        f.runner
            .state()
            .objects_that_dealt_damage
            .contains(&f.source),
        !returned
    );
    assert_eq!(f.runner.life(P0), 20);
    assert_eq!(f.runner.life(P1), 20 + damage as i32);
    assert_eq!(f.runner.life(P2), 20 - damage as i32);
}

fn u13_change_source(
    f: &mut U13Fixture,
    child: ObjectId,
    returned: bool,
    absent: bool,
) -> ObjectIncarnationRef {
    let old = ObjectIncarnationRef::from_object(&f.runner.state().objects[&f.source]);
    u13_response(&mut f.runner, f.control, f.source, Some(P1));
    assert_eq!(f.runner.state().objects[&f.source].controller, P1);
    assert_eq!(
        f.runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == child)
            .unwrap()
            .controller,
        P0
    );
    if returned {
        u13_response(&mut f.runner, f.blink, f.source, None);
        u13_until(&mut f.runner, |state| {
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
        });
        prompt(&f.runner, 1);
        f.runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
        let new = ObjectIncarnationRef::from_object(&f.runner.state().objects[&f.source]);
        assert_ne!(old, new);
        assert_eq!(f.runner.state().objects[&f.source].controller, P0);
        for keyword in [
            engine::types::keywords::Keyword::Lifelink,
            engine::types::keywords::Keyword::Indestructible,
        ] {
            assert!(!f.runner.state().objects[&f.source]
                .keywords
                .contains(&keyword));
        }
        assert!(!f.runner.state().objects[&f.source]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Artifact));
        if absent {
            // Supplementary isolation only after the genuine public departure,
            // return, and new entrant's independent parent have all settled.
            f.runner.state_mut().objects.remove(&f.source);
        }
    } else {
        u13_response(&mut f.runner, f.live_grant, f.source, None);
    }
    old
}

#[test]
fn u13_public_printed_keywords_departed_source_owner_controller_and_current_twin() {
    for (returned, absent) in [(false, false), (true, false), (true, true)] {
        let mut f = u13_fixture(IMMARD, 2, |_, _, _| {});
        let child = u13_keyword_then_attack(&mut f);
        let old = u13_change_source(&mut f, child, returned, absent);
        finish(&mut f.runner, &mut Vec::new());
        u13_damage_record(&f, old, 4, returned);
        if !returned {
            assert!(f
                .runner
                .state()
                .damage_dealt_this_turn
                .last()
                .unwrap()
                .source_keywords
                .contains(&engine::types::keywords::Keyword::Flying));
        }
    }
}

fn u13_install_damage_modifier(
    s: &mut GameScenario,
    pending: &mut Vec<ReplacementDefinition>,
    state_level: bool,
    controller: PlayerId,
    name: &str,
    modifier: engine::types::ability::DamageModification,
    filter: Option<TargetFilter>,
) -> engine::types::proposed_event::ReplacementId {
    let mut builder = s.add_creature(controller, name, 2, 2);
    let host = builder.id();
    let mut definition = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .description(name.into())
        .damage_modification(modifier);
    definition.damage_source_filter = filter;
    if state_level {
        definition.source_object = Some(host);
        definition.source_controller = Some(controller);
        let index = pending.len();
        pending.push(definition);
        engine::types::proposed_event::ReplacementId {
            source: ObjectId(0),
            index,
        }
    } else {
        builder.with_replacement_definition(definition);
        engine::types::proposed_event::ReplacementId {
            source: host,
            index: 0,
        }
    }
}

fn u13_control_filter(artifact: bool) -> TargetFilter {
    TargetFilter::Typed(engine::types::ability::TypedFilter {
        type_filters: if artifact {
            vec![engine::types::ability::TypeFilter::Artifact]
        } else {
            vec![]
        },
        controller: Some(engine::types::ability::ControllerRef::You),
        properties: vec![],
    })
}

#[test]
fn u13_public_exact_source_filtered_damage_object_and_state_installers() {
    use engine::types::ability::DamageModification;
    // CR 614.1a + CR 608.2h: replacement "you" is its installer, while the
    // damage subject is the departed/current original source, independently.
    for state_level in [false, true] {
        for (installer, returned, artifact, expected) in [
            (P1, true, false, 8),
            (P0, true, false, 4),
            (P1, false, false, 8),
            (P1, true, true, 8),
        ] {
            let mut f = u13_fixture(IMMARD, 2, |s, _, pending| {
                u13_install_damage_modifier(
                    s,
                    pending,
                    state_level,
                    installer,
                    "U13 filtered Double",
                    DamageModification::Double,
                    Some(u13_control_filter(artifact)),
                );
            });
            let child = u13_keyword_then_attack(&mut f);
            let old = u13_change_source(&mut f, child, returned, false);
            finish(&mut f.runner, &mut Vec::new());
            u13_damage_record(&f, old, expected, returned);
        }
        // Positive reach guard for the wrong-A refusal: A's CURRENT source
        // qualifies for A's source filter and doubles ordinary public damage.
        let mut f = u13_fixture(IMMARD, 1, |s, _, pending| {
            u13_install_damage_modifier(
                s,
                pending,
                state_level,
                P0,
                "U13 current A Double",
                DamageModification::Double,
                Some(u13_control_filter(false)),
            );
        });
        u13_remove_and_announce(&mut f.runner, f.source, 0, P2);
        finish(&mut f.runner, &mut Vec::new());
        assert_eq!(f.runner.life(P2), 12);
    }
}

#[test]
fn u13_public_filtered_and_unfiltered_replacement_prompts_reselect_reload_seven() {
    use engine::types::ability::DamageModification;
    for state_level in [false, true] {
        for filtered in [false, true] {
            let mut selected = Vec::new();
            let mut hostile = None;
            let mut f = u13_fixture(IMMARD, 2, |s, _, pending| {
                selected.push(u13_install_damage_modifier(
                    s,
                    pending,
                    state_level,
                    P1,
                    "U13 Minus",
                    DamageModification::Minus { value: 1 },
                    None,
                ));
                selected.push(u13_install_damage_modifier(
                    s,
                    pending,
                    state_level,
                    P1,
                    "U13 Double",
                    DamageModification::Double,
                    filtered.then(|| u13_control_filter(false)),
                ));
                selected.push(u13_install_damage_modifier(
                    s,
                    pending,
                    state_level,
                    P1,
                    "U13 Plus",
                    DamageModification::Plus {
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    None,
                ));
                if filtered {
                    hostile = Some(u13_install_damage_modifier(
                        s,
                        pending,
                        state_level,
                        P0,
                        "U13 wrong A Double",
                        DamageModification::Double,
                        Some(u13_control_filter(false)),
                    ));
                }
            });
            let child = u13_keyword_then_attack(&mut f);
            let old = u13_change_source(&mut f, child, true, false);
            u13_until(&mut f.runner, |state| {
                matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
            });
            let mut replacement_events = Vec::new();
            for (round, chosen) in [selected[0], selected[1]].into_iter().enumerate() {
                let WaitingFor::ReplacementChoice { player, .. } = f.runner.state().waiting_for
                else {
                    panic!("genuine re-park required");
                };
                assert_eq!(player, P2);
                let pending = f.runner.state().pending_replacement.as_ref().unwrap();
                assert!(
                    pending.candidates.contains(&selected[1]),
                    "B Double must qualify at every consult"
                );
                assert!(hostile.is_none_or(|hostile| !pending.candidates.contains(&hostile)));
                assert!(!pending.candidates.is_empty());
                assert_eq!(pending.proposed.already_applied(&selected[0]), round == 1);
                assert!(!pending.proposed.already_applied(&selected[1]));
                assert_eq!(
                    pending
                        .damage_source_context
                        .as_ref()
                        .unwrap()
                        .identity
                        .reference,
                    old
                );
                assert!(
                    matches!(&pending.proposed, engine::types::proposed_event::ProposedEvent::Damage { amount, .. }
                    if *amount == if round == 0 { 4 } else { 3 })
                );
                let chooser =
                    engine::game::visibility::filter_state_for_viewer(f.runner.state(), P2);
                assert!(chooser
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .damage_source_context
                    .is_none());
                f.runner = reload(f.runner, true);
                let pending = f.runner.state().pending_replacement.as_ref().unwrap();
                let index = pending
                    .candidates
                    .iter()
                    .position(|candidate| *candidate == chosen)
                    .unwrap();
                assert!(engine::ai_support::legal_actions(f.runner.state()).iter()
                    .any(|action| matches!(action, GameAction::ChooseReplacement { index: offered } if *offered == index)));
                assert!(f
                    .runner
                    .act(GameAction::ChooseReplacement { index: usize::MAX })
                    .is_err());
                assert_eq!(
                    f.runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .unwrap()
                        .damage_source_context
                        .as_ref()
                        .unwrap()
                        .identity
                        .reference,
                    old
                );
                replacement_events.extend(
                    f.runner
                        .act(GameAction::ChooseReplacement { index })
                        .unwrap()
                        .events,
                );
            }
            finish(&mut f.runner, &mut replacement_events);
            for id in selected.iter().filter(|_| !state_level) {
                assert_eq!(replacement_events.iter().filter(|event| matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == id.source)).count(), 1);
            }
            // CR 614.5 + CR 616.1f: 4−1=3, doubled to 6, plus 1=7.
            u13_damage_record(&f, old, 7, true);
        }
    }
}

#[test]
fn u13_public_relevant_removal_replacement_pause_reload_and_prevention() {
    for (prevent, persistence) in [
        (false, 0),
        (true, 0),
        (false, 1),
        (true, 1),
        (false, 2),
        (true, 2),
        (false, 3),
        (true, 3),
    ] {
        let mut ids = Vec::new();
        let mut f = u13_fixture(IMMARD, 2, |s, source, _| {
            for (name, modification) in [
                (
                    "U13 removal Double",
                    QuantityModification::Times { factor: 2 },
                ),
                (
                    "U13 removal other",
                    if prevent {
                        QuantityModification::Prevent
                    } else {
                        QuantityModification::Minus { value: 1 }
                    },
                ),
            ] {
                ids.push(
                    s.add_creature(P0, name, 2, 2)
                        .with_replacement_definition(
                            ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                                .valid_card(TargetFilter::SpecificObject { id: source })
                                .quantity_modification(modification),
                        )
                        .id(),
                );
            }
        });
        f.runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
        assert!(matches!(
            f.runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let pending = f.runner.state().pending_replacement.as_ref().unwrap();
        assert!(matches!(
            &pending.proposed,
            engine::types::proposed_event::ProposedEvent::RemoveCounter {
                result_binding: Some(_),
                ..
            }
        ));
        let selected = ids[usize::from(prevent)];
        f.runner = u13_reload_kind(f.runner, persistence);
        let index = f
            .runner
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .candidates
            .iter()
            .position(|candidate| candidate.source == selected)
            .unwrap();
        let action = f
            .runner
            .act(GameAction::ChooseReplacement { index })
            .unwrap();
        if prevent {
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(n(&f.runner, f.source, &charge()), 2);
            assert_eq!(event_count(&action.events, f.source, &charge(), false), 0);
            assert_eq!(f.runner.life(P2), 20);
        } else {
            u13_until(&mut f.runner, |state| {
                matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
            });
            assert_eq!(n(&f.runner, f.source, &charge()), 1);
            f.runner
                .act(GameAction::SelectModes { indices: vec![1] })
                .unwrap();
            finish(&mut f.runner, &mut Vec::new());
            assert!(f.runner.state().objects[&f.source]
                .keywords
                .contains(&engine::types::keywords::Keyword::Lifelink));
        }
    }
}

#[test]
fn u13_current_removal_pause_requires_both_tickets_from_its_owned_reader() {
    use engine::types::ability::AbilityCondition;
    use engine::types::game_state::{
        InstructionResult, PersistedGameState, TrustedGameStateEnvelope,
    };
    use engine::types::resolution::ResolutionFrame;

    for persistence in 0..5 {
        for mode in [0, 1] {
            let mut selected = ObjectId(0);
            let mut f = u13_fixture(IMMARD, 2, |s, source, _| {
                for (name, modification) in [
                    (
                        "Synthetic ticket Double",
                        QuantityModification::Times { factor: 2 },
                    ),
                    (
                        "Synthetic ticket Minus",
                        QuantityModification::Minus { value: 1 },
                    ),
                ] {
                    let id = s
                        .add_creature(P0, name, 2, 2)
                        .with_replacement_definition(
                            ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                                .valid_card(TargetFilter::SpecificObject { id: source })
                                .quantity_modification(modification),
                        )
                        .id();
                    if name == "Synthetic ticket Double" {
                        selected = id;
                    }
                }
            });
            f.runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
            assert!(matches!(
                f.runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            let queue = f.runner.state().active_counter_removals().unwrap();
            assert!(queue.in_flight.is_some());
            let binding = queue.result_binding.as_ref().unwrap();
            let engine::types::proposed_event::ProposedEvent::RemoveCounter {
                result_binding: Some(proposed_binding),
                ..
            } = &f
                .runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .proposed
            else {
                panic!("genuine bound removal proposal")
            };
            assert_eq!(proposed_binding, binding);
            let InstructionResult::CounterRemovals(result) = &f.runner.state().return_result_frames
                [&binding.occurrence]
                .results[&binding.result]
            else {
                panic!("present typed counter result")
            };
            assert!(result.counter_removals.is_empty());
            assert_eq!(result.source, binding.source);
            assert!(f.runner.state().resolution_stack.iter().any(|frame| {
                let ResolutionFrame::AbilityContinuation(frame) = frame else {
                    return false;
                };
                let reader = &frame.pending;
                let is_owned = |condition: &AbilityCondition| {
                    matches!(condition,
                    AbilityCondition::CounterRemovedThisWay { result, counters }
                        if *result == binding.result && *counters == binding.counters)
                };
                reader.return_result_occurrence == Some(binding.occurrence)
                    && reader.chain.source_id == binding.source.reference.object_id
                    && reader
                        .chain
                        .condition
                        .as_ref()
                        .is_some_and(|condition| match condition {
                            AbilityCondition::And { conditions } => conditions.iter().any(is_owned),
                            condition => is_owned(condition),
                        })
            }));
            let good = match persistence {
                0 => serde_json::to_value(f.runner.state()).unwrap(),
                1 => serde_json::to_value(ResolutionStateWire::from_game_state(
                    f.runner.state().clone(),
                ))
                .unwrap(),
                2 => serde_json::to_value(PersistedGameState::Raw(Box::new(
                    f.runner.state().clone(),
                )))
                .unwrap(),
                3 => serde_json::to_value(PersistedGameState::capture(f.runner.state().clone()))
                    .unwrap(),
                4 => serde_json::to_value(TrustedGameStateEnvelope::capture(
                    f.runner.state().clone(),
                ))
                .unwrap(),
                _ => unreachable!(),
            };
            for omitted in 0..3 {
                let mut bad = good.clone();
                let state = if persistence >= 3 {
                    &mut bad["state"]
                } else {
                    &mut bad
                };
                // Keep the exact reader/result untouched; omit both carriers,
                // then each single carrier as coherence controls.
                if omitted != 2 {
                    assert!(state["pending_replacement"]["proposed"]
                        .as_object_mut()
                        .unwrap()
                        .remove("result_binding")
                        .is_some());
                }
                if omitted != 1 {
                    let carrier = if persistence == 0 {
                        "resolution_stack"
                    } else {
                        "resolution_frames"
                    };
                    let queue = state[carrier]["frames"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|frame| frame["type"] == "CounterRemovals")
                        .unwrap();
                    assert!(queue["data"]
                        .as_object_mut()
                        .unwrap()
                        .remove("result_binding")
                        .is_some());
                }
                let rejected = match persistence {
                    0 => serde_json::from_value::<GameState>(bad).is_err(),
                    1 => serde_json::from_value::<ResolutionStateWire>(bad).is_err(),
                    2 | 3 => serde_json::from_value::<PersistedGameState>(bad).is_err(),
                    4 => serde_json::from_value::<TrustedGameStateEnvelope>(bad).is_err(),
                    _ => unreachable!(),
                };
                assert!(rejected, "current ingress {persistence}, omitted {omitted}");
            }
            let state = match persistence {
                0 => serde_json::from_value::<GameState>(good).unwrap(),
                1 => serde_json::from_value::<ResolutionStateWire>(good)
                    .unwrap()
                    .into_game_state(),
                2 | 3 => serde_json::from_value::<PersistedGameState>(good)
                    .unwrap()
                    .into_game_state()
                    .unwrap(),
                4 => serde_json::from_value::<TrustedGameStateEnvelope>(good)
                    .unwrap()
                    .into_game_state(),
                _ => unreachable!(),
            };
            f.runner = GameRunner::from_state(state);
            let index = f
                .runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .candidates
                .iter()
                .position(|id| id.source == selected)
                .unwrap();
            f.runner
                .act(GameAction::ChooseReplacement { index })
                .unwrap();
            u13_until(&mut f.runner, |state| {
                matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
            });
            assert_eq!(n(&f.runner, f.source, &charge()), 1);
            // CR 603.12: accepted actual removal earns the independent child.
            f.runner
                .act(GameAction::SelectModes {
                    indices: vec![mode],
                })
                .unwrap();
            if mode == 0 {
                let action = engine::ai_support::legal_actions(f.runner.state()).into_iter().find(|action|
                    matches!(action, GameAction::ChooseTarget { target: Some(engine::types::ability::TargetRef::Player(player)) } if *player == P2)).unwrap();
                f.runner.act(action).unwrap();
            }
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(f.runner.life(P2), if mode == 0 { 16 } else { 20 });
            if mode == 1 {
                assert!(f.runner.state().objects[&f.source]
                    .keywords
                    .contains(&engine::types::keywords::Keyword::Lifelink));
                assert!(f.runner.state().objects[&f.source]
                    .keywords
                    .contains(&engine::types::keywords::Keyword::Indestructible));
            }
            assert!(f.runner.state().return_result_frames.is_empty());
        }
    }
}

#[test]
fn u13_paused_exact_recipient_and_current_missing_wrong_ticket_refusal() {
    for wire in [false, true] {
        let f = u13_fixture(IMMARD, 1, |_, _, _| {});
        let good = serde_json::to_value(f.runner.state()).unwrap();
        assert!(good["waiting_for"]["context"]["counter_removal_result"].is_object());
        for fault in 0..8 {
            let mut bad = good.clone();
            let binding = &mut bad["waiting_for"]["context"]["counter_removal_result"];
            match fault {
                0 => *binding = serde_json::Value::Null,
                1 => binding["occurrence"] = serde_json::json!(u64::MAX),
                2 => binding["result"] = serde_json::json!(u32::MAX),
                3 => binding["source"]["reference"]["incarnation"] = serde_json::json!(999999),
                4 => binding["recipient"]["incarnation"] = serde_json::json!(999999),
                5 => {
                    binding["counters"] = serde_json::to_value(CounterMatch::OfType(oil())).unwrap()
                }
                6 => binding
                    .as_object_mut()
                    .unwrap()
                    .remove("recipient")
                    .map(|_| ())
                    .unwrap(),
                7 => {
                    let frames = bad["return_result_frames"].as_object_mut().unwrap();
                    let frame = frames.values_mut().next().unwrap().as_object_mut().unwrap();
                    *frame
                        .get_mut("results")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .values_mut()
                        .next()
                        .unwrap() = serde_json::json!([]);
                }
                _ => unreachable!(),
            }
            assert!(
                serde_json::from_value::<GameState>(bad).is_err(),
                "fault {fault}"
            );
        }
        let source = f.source;
        let mut r = reload(f.runner, wire);
        let old = ObjectIncarnationRef::from_object(&r.state().objects[&source]);
        engine::game::zones::move_to_zone(r.state_mut(), source, Zone::Exile, &mut Vec::new());
        engine::game::zones::move_to_zone(
            r.state_mut(),
            source,
            Zone::Battlefield,
            &mut Vec::new(),
        );
        assert_ne!(
            old,
            ObjectIncarnationRef::from_object(&r.state().objects[&source])
        );
        // Supplementary same-id isolation begins only after a genuine public
        // adjustment prompt and trusted reload; the original action still runs.
        r.act(GameAction::ChooseBranch { index: 0 }).unwrap();
        finish(&mut r, &mut Vec::new());
        assert_eq!(n(&r, source, &charge()), 0);
        assert_eq!(r.life(P2), 20);
        let mut twin = u13_fixture(IMMARD, 1, |_, _, _| {});
        twin.runner = reload(twin.runner, wire);
        u13_remove_and_announce(&mut twin.runner, twin.source, 0, P2);
        finish(&mut twin.runner, &mut Vec::new());
        assert_eq!(twin.runner.life(P2), 16);
    }
}

#[test]
fn u13_public_keywords_duration_and_returned_recipient_refusal() {
    for returned in [false, true] {
        let mut f = u13_fixture(IMMARD, 1, |_, _, _| {});
        let child = u13_remove_and_announce(&mut f.runner, f.source, 1, P2);
        if returned {
            u13_response(&mut f.runner, f.blink, f.source, None);
            u13_until(&mut f.runner, |state| {
                matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
            });
            f.runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
            assert!(f.runner.state().stack.iter().any(|entry| entry.id == child));
        }
        finish(&mut f.runner, &mut Vec::new());
        for keyword in [
            engine::types::keywords::Keyword::Lifelink,
            engine::types::keywords::Keyword::Indestructible,
        ] {
            assert_eq!(
                f.runner.state().objects[&f.source]
                    .keywords
                    .contains(&keyword),
                !returned
            );
        }
        f.runner.advance_to_phase(Phase::Cleanup);
        for keyword in [
            engine::types::keywords::Keyword::Lifelink,
            engine::types::keywords::Keyword::Indestructible,
        ] {
            assert!(!f.runner.state().objects[&f.source]
                .keywords
                .contains(&keyword));
        }
    }
}

#[test]
fn u13_public_singular_event_multiplicity_announces_each_child_once() {
    let text = "When this creature enters, put two charge counters on it or remove two from it. When you remove a counter this way, choose one —\n• You gain 1 life.\n• You gain 2 life.";
    for remove in [false, true] {
        let mut f = u13_fixture(text, 2, |_, _, _| {});
        let action = f
            .runner
            .act(GameAction::ChooseBranch {
                index: if remove { 0 } else { 1 },
            })
            .unwrap();
        assert_eq!(
            event_count(&action.events, f.source, &charge(), false),
            if remove { 2 } else { 0 }
        );
        let mut announced = 0;
        for _ in 0..70 {
            match f.runner.state().waiting_for {
                WaitingFor::AbilityModeChoice { .. } => {
                    announced += 1;
                    f.runner
                        .act(GameAction::SelectModes { indices: vec![0] })
                        .unwrap();
                }
                WaitingFor::Priority { .. } if f.runner.state().stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    f.runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected singular-event child: {other:?}"),
            }
        }
        assert_eq!(announced, if remove { 2 } else { 0 });
        assert_eq!(f.runner.life(P0), if remove { 22 } else { 20 });
        assert!(f.runner.state().return_result_frames.is_empty());
    }
}

#[test]
fn u13_public_child_mode_target_and_trusted_roundtrips() {
    for persistence in 0..4 {
        let mut f = u13_fixture(IMMARD, 1, |_, _, _| {});
        let restore = |r: GameRunner| -> GameRunner {
            let state = match persistence {
                0 => serde_json::from_str::<GameState>(&serde_json::to_string(r.state()).unwrap())
                    .unwrap(),
                1 => serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone()))
                        .unwrap(),
                )
                .unwrap()
                .into_game_state(),
                2 => serde_json::from_value::<engine::types::game_state::PersistedGameState>(
                    serde_json::to_value(engine::types::game_state::PersistedGameState::capture(
                        r.state().clone(),
                    ))
                    .unwrap(),
                )
                .unwrap()
                .into_game_state()
                .unwrap(),
                3 => serde_json::from_value::<engine::types::game_state::TrustedGameStateEnvelope>(
                    serde_json::to_value(
                        engine::types::game_state::TrustedGameStateEnvelope::capture(
                            r.state().clone(),
                        ),
                    )
                    .unwrap(),
                )
                .unwrap()
                .into_game_state(),
                _ => unreachable!(),
            };
            GameRunner::from_state(state)
        };
        f.runner = restore(f.runner);
        f.runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
        u13_until(&mut f.runner, |state| {
            matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
        });
        f.runner = restore(f.runner);
        f.runner
            .act(GameAction::SelectModes { indices: vec![0] })
            .unwrap();
        assert!(matches!(
            f.runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        ));
        f.runner = restore(f.runner);
        let target = engine::ai_support::legal_actions(f.runner.state()).into_iter().find(|action|
            matches!(action, GameAction::ChooseTarget { target: Some(engine::types::ability::TargetRef::Player(player)) } if *player == P2)).unwrap();
        f.runner.act(target).unwrap();
        finish(&mut f.runner, &mut Vec::new());
        assert_eq!(f.runner.life(P2), 16);
        assert_eq!(n(&f.runner, f.source, &charge()), 0);
        assert!(f.runner.state().return_result_frames.is_empty());
    }
}
fn charge() -> CounterType {
    CounterType::Generic("charge".into())
}
fn oil() -> CounterType {
    CounterType::Generic("oil".into())
}
fn n(r: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    r.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}
fn mana(s: &mut GameScenario) {
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]); 20],
    );
}
fn reload(r: GameRunner, wire: bool) -> GameRunner {
    let state = if wire {
        let encoded =
            serde_json::to_string(&ResolutionStateWire::from_game_state(r.state().clone()))
                .unwrap();
        serde_json::from_str::<ResolutionStateWire>(&encoded)
            .unwrap()
            .into_game_state()
    } else {
        serde_json::from_str::<GameState>(&serde_json::to_string(r.state()).unwrap()).unwrap()
    };
    GameRunner::from_state(state)
}
fn finish(r: &mut GameRunner, events: &mut Vec<GameEvent>) {
    let mut done = false;
    for _ in 0..40 {
        match r.state().waiting_for {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => {
                done = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                events.extend(r.act(GameAction::PassPriority).unwrap().events)
            }
            ref other => panic!("unexpected pending tail: {other:?}"),
        }
    }
    assert!(done, "resolution must drain");
    assert!(
        r.state().resolution_stack.is_empty(),
        "no paused branch/tail remains"
    );
}
fn prompt(r: &GameRunner, expected: usize) {
    let WaitingFor::ChooseOneOfBranch {
        player,
        branches,
        branch_descriptions,
        parent_occurrences,
        ..
    } = &r.state().waiting_for
    else {
        panic!(
            "actual adjustment prompt required: {:?}",
            r.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(branches.len(), expected);
    assert_eq!(branch_descriptions.len(), expected);
    let occurrences = parent_occurrences.occurrences();
    let [(engine::types::ability::TargetRef::Object(id), Some(pin))] = occurrences.as_slice()
    else {
        panic!("exact singleton pinned recipient: {occurrences:?}")
    };
    assert_eq!(
        *pin,
        ObjectIncarnationRef::from_object(&r.state().objects[id])
    );
    let actions = engine::ai_support::legal_actions(r.state());
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, GameAction::ChooseBranch { .. }))
            .count(),
        expected
    );
    assert!(actions
        .iter()
        .all(|a| matches!(a, GameAction::ChooseBranch { .. })));
}
fn event_count(events: &[GameEvent], id: ObjectId, kind: &CounterType, add: bool) -> u32 {
    events
        .iter()
        .map(|e| match e {
            GameEvent::CounterAdded {
                object_id,
                counter_type,
                count,
                ..
            } if add && *object_id == id && counter_type == kind => *count,
            GameEvent::CounterRemoved {
                object_id,
                counter_type,
                count,
            } if !add && *object_id == id && counter_type == kind => *count,
            _ => 0,
        })
        .sum()
}
fn choose(r: &mut GameRunner, index: usize) -> Vec<GameEvent> {
    let mut events = r.act(GameAction::ChooseBranch { index }).unwrap().events;
    finish(r, &mut events);
    events
}
fn block(s: &mut GameScenario, recipient: ObjectId, remove: bool, add: bool) {
    if remove {
        s.add_creature(P0, "Synthetic removal prohibition", 2, 2)
            .with_static_definition(
                StaticDefinition::new(StaticMode::CountersCantBeRemoved {
                    counter_type: charge(),
                })
                .affected(TargetFilter::SpecificObject { id: recipient }),
            );
    }
    if add {
        s.add_creature(P0, "Synthetic placement prohibition", 2, 2)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .valid_card(TargetFilter::SpecificObject { id: recipient })
                    .counter_match(CounterMatch::OfType(charge()))
                    .quantity_modification(QuantityModification::Prevent),
            );
    }
}
fn public_spell(
    text: &str,
    amount: Option<u32>,
    initial: u32,
    prohibitions: (bool, bool),
) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let recipient = s.add_creature(P0, "Recipient", 2, 4).id();
    let decoy = s.add_creature(P0, "Other legal recipient", 3, 4).id();
    s.with_counter(recipient, charge(), initial)
        .with_counter(recipient, oil(), 2)
        .with_counter(decoy, charge(), 8);
    block(&mut s, recipient, prohibitions.0, prohibitions.1);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Synthetic mandatory adjustment", true, text)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: if amount.is_some() {
                vec![ManaCostShard::X, ManaCostShard::Blue]
            } else {
                vec![ManaCostShard::Blue]
            },
        })
        .id();
    let mut r = s.build();
    if let Some(x) = amount {
        let committed = r.cast(spell).x(x).target_object(recipient).commit();
        assert_eq!(
            committed
                .state()
                .stack
                .iter()
                .find(|entry| entry.source_id == spell)
                .unwrap()
                .ability()
                .unwrap()
                .chosen_x,
            Some(x)
        );
        let outcome = committed.resolve();
        outcome.assert_life_delta(P0, 0);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::ChooseOneOfBranch { .. }
        ));
    } else {
        let outcome = r.cast(spell).target_object(recipient).resolve();
        outcome.assert_life_delta(P0, 0);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::ChooseOneOfBranch { .. }
        ));
    }
    (r, recipient, decoy)
}

#[test]
fn public_x_three_and_fixed_three_bind_before_prompt_and_trusted_reload() {
    // CR 107.3a + CR 107.3i + CR 608.2h: the announced X survives branch reconstruction.
    for (text, x) in [(X_TEXT, Some(3)), (THREE_TEXT, None)] {
        for (index, initial, expected) in [(0, 5, 2), (1, 2, 5)] {
            for wire in [false, true] {
                let (mut r, id, decoy) = public_spell(text, x, initial, (false, false));
                prompt(&r, 2);
                assert_eq!(r.life(P0), 20);
                assert_eq!(n(&r, id, &charge()), initial);
                if let WaitingFor::ChooseOneOfBranch {
                    branches,
                    branch_descriptions,
                    ..
                } = &r.state().waiting_for
                {
                    for branch in branches {
                        assert!(matches!(
                            branch.effect.as_ref(),
                            Effect::PutCounter {
                                count: QuantityExpr::Fixed { value: 3 },
                                ..
                            } | Effect::RemoveCounter {
                                count: QuantityExpr::Fixed { value: 3 },
                                ..
                            }
                        ));
                    }
                    assert!(branch_descriptions[0].contains("remove 3"));
                    assert!(branch_descriptions[1].contains("put 3"));
                }
                r = reload(r, wire);
                prompt(&r, 2);
                let events = choose(&mut r, index);
                assert_eq!(n(&r, id, &charge()), expected);
                assert_eq!(n(&r, id, &oil()), 2);
                assert_eq!(n(&r, decoy, &charge()), 8);
                assert_eq!(event_count(&events, id, &charge(), index == 1), 3);
                assert_eq!(event_count(&events, id, &charge(), index != 1), 0);
                assert_eq!(r.life(P0), 21);
            }
        }
    }
}
#[test]
fn public_zero_x_fixed_zero_and_positive_feasibility_twins() {
    // CR 107.1b + CR 608.2d: zero removal needs no positive counter.
    for (text, x) in [(X_TEXT, Some(0)), (ZERO_TEXT, None)] {
        for initial in [0, 5] {
            for prohibitions in [(false, false), (true, false), (false, true), (true, true)] {
                for index in [0, 1] {
                    let (mut r, id, decoy) = public_spell(text, x, initial, prohibitions);
                    prompt(&r, 2);
                    if let WaitingFor::ChooseOneOfBranch {
                        branch_descriptions,
                        ..
                    } = &r.state().waiting_for
                    {
                        assert!(branch_descriptions[0].contains("remove 0"));
                        assert!(branch_descriptions[1].contains("put 0"));
                    }
                    r = reload(r, index == 1);
                    let events = choose(&mut r, index);
                    assert!(!events.iter().any(|e| matches!(
                        e,
                        GameEvent::CounterAdded { .. } | GameEvent::CounterRemoved { .. }
                    )));
                    assert_eq!(n(&r, id, &charge()), initial);
                    assert_eq!(n(&r, id, &oil()), 2);
                    assert_eq!(n(&r, decoy, &charge()), 8);
                    assert_eq!(event_count(&events, id, &charge(), true), 0);
                    assert_eq!(event_count(&events, id, &charge(), false), 0);
                    assert_eq!(r.life(P0), 21);
                }
            }
        }
    }
    let (mut r, id, _) = public_spell(X_TEXT, Some(1), 0, (false, false));
    prompt(&r, 1);
    let events = choose(&mut r, 0);
    assert_eq!(n(&r, id, &charge()), 1);
    assert_eq!(event_count(&events, id, &charge(), true), 1);
    assert_eq!(r.life(P0), 21);
    for (remove, add, index, expected) in [(true, false, 0, 6), (false, true, 0, 4)] {
        let (mut r, id, _) = public_spell(X_TEXT, Some(1), 5, (remove, add));
        prompt(&r, 1);
        let events = choose(&mut r, index);
        assert_eq!(n(&r, id, &charge()), expected);
        assert_eq!(event_count(&events, id, &charge(), !add), 1);
    }
}

fn loaded_spell(
    count: QuantityExpr,
    initial: u32,
    power: i32,
    counters: CounterMatch,
    prohibitions: (bool, bool),
) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let id = s
        .add_creature(P0, "Synthetic calculated recipient", power, 8)
        .id();
    let decoy = s.add_creature(P0, "Synthetic decoy", 3, 8).id();
    s.with_counter(id, charge(), initial)
        .with_counter(id, oil(), 2)
        .with_counter(decoy, charge(), 8);
    block(&mut s, id, prohibitions.0, prohibitions.1);
    let target = engine::parser::parse_oracle_text(
        "Put a charge counter on target creature.",
        "Synthetic",
        &[],
        &["Instant".into()],
        &[],
    )
    .abilities[0]
        .effect
        .target_filter()
        .unwrap()
        .clone();
    let mut def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChooseCounterAdjustment {
            adjustment: CounterAdjustment::AddOrRemove,
            counters,
            count,
            target,
        },
    );
    def.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )));
    // Explicitly synthetic supported loaded-definition route; no parsed AST replacement.
    let def: AbilityDefinition =
        serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
    let spell = s
        .add_spell_to_hand(P0, "Synthetic loaded adjustment", true)
        .with_ability_definition(def)
        .id();
    let mut r = s.build();
    let outcome = r.cast(spell).target_object(id).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    outcome.assert_life_delta(P0, 0);
    (r, id, decoy)
}
#[test]
fn loaded_calculations_clamp_negative_before_branch_persistence() {
    let recipient_power = || QuantityExpr::Ref {
        qty: QuantityRef::Power {
            scope: ObjectScope::Recipient,
        },
    };
    for (expr, power, bound) in [
        (recipient_power(), -1, 0),
        (recipient_power(), 0, 0),
        (recipient_power(), 2, 2),
        (
            QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Fixed { value: 2 }),
                offset: -3,
            },
            2,
            0,
        ),
    ] {
        for index in [0, 1] {
            for wire in [false, true] {
                let (mut r, id, decoy) = loaded_spell(
                    expr.clone(),
                    5,
                    power,
                    CounterMatch::OfType(charge()),
                    (false, false),
                );
                prompt(&r, 2);
                if let WaitingFor::ChooseOneOfBranch {
                    branches,
                    branch_descriptions,
                    ..
                } = &r.state().waiting_for
                {
                    for branch in branches {
                        let c = match branch.effect.as_ref() {
                            Effect::RemoveCounter { count, .. }
                            | Effect::PutCounter { count, .. } => count,
                            _ => panic!(),
                        };
                        assert_eq!(*c, QuantityExpr::Fixed { value: bound });
                    }
                    assert!(branch_descriptions
                        .iter()
                        .all(|label| label.contains(&bound.to_string())));
                }
                r = reload(r, wire);
                let events = choose(&mut r, index);
                if bound == 0 {
                    assert!(!events.iter().any(|e| matches!(
                        e,
                        GameEvent::CounterAdded { .. } | GameEvent::CounterRemoved { .. }
                    )));
                }
                assert_eq!(
                    n(&r, id, &charge()),
                    if index == 0 {
                        5 - bound as u32
                    } else {
                        5 + bound as u32
                    }
                );
                assert_eq!(
                    event_count(&events, id, &charge(), index == 1),
                    bound as u32
                );
                assert_eq!(event_count(&events, id, &charge(), index != 1), 0);
                assert_eq!(n(&r, id, &oil()), 2);
                assert_eq!(n(&r, decoy, &charge()), 8);
                assert_eq!(r.life(P0), 21);
            }
        }
    }
}
#[test]
fn loaded_explicit_sentinel_and_outer_up_to_keep_ordinary_removal_routes() {
    for initial in [0, 5] {
        for index in [0, 1] {
            let (mut r, id, _) = loaded_spell(
                QuantityExpr::Fixed { value: -1 },
                initial,
                2,
                CounterMatch::OfType(charge()),
                (false, false),
            );
            prompt(&r, 2);
            r = reload(r, true);
            let events = choose(&mut r, index);
            assert_eq!(n(&r, id, &charge()), if index == 0 { 0 } else { initial });
            assert_eq!(n(&r, id, &oil()), 2);
            assert_eq!(
                event_count(&events, id, &charge(), false),
                if index == 0 { initial } else { 0 }
            );
            assert_eq!(event_count(&events, id, &charge(), true), 0);
            assert_eq!(r.life(P0), 21);
        }
    }
    for (initial, selection, blocked) in [
        (5, 2, false),
        (5, 0, false),
        (0, 0, false),
        (5, 0, true),
        (5, 2, true),
    ] {
        let (mut r, id, _) = loaded_spell(
            QuantityExpr::up_to(QuantityExpr::Fixed { value: -1 }),
            initial,
            2,
            CounterMatch::OfType(charge()),
            (blocked, false),
        );
        prompt(&r, 2);
        r = reload(r, false);
        let mut events = r.act(GameAction::ChooseBranch { index: 0 }).unwrap().events;
        if initial > 0 {
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::RemoveCountersChoice { .. }
            ));
            assert_eq!(r.life(P0), 20);
            r = reload(r, true);
            events.extend(
                r.act(GameAction::ChooseCountersToRemove {
                    selections: if selection == 0 {
                        vec![]
                    } else {
                        vec![CounterRemoveChoice {
                            counter_type: charge(),
                            count: selection,
                        }]
                    },
                })
                .unwrap()
                .events,
            );
        }
        finish(&mut r, &mut events);
        let removed = if blocked { 0 } else { selection };
        assert_eq!(n(&r, id, &charge()), initial - removed);
        assert_eq!(event_count(&events, id, &charge(), false), removed);
        assert_eq!(n(&r, id, &oil()), 2);
        assert_eq!(r.life(P0), 21);
    }
}
#[test]
fn public_source_parent_excerpt_adds_first_counter_and_keeps_other_kinds() {
    for initial in [0, 2] {
        for add in [false, true] {
            if initial == 0 && !add {
                continue;
            }
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let other = s.add_creature(P0, "Immard, the Stormcleaver", 4, 4).id();
            let source = s
                .add_creature_to_hand_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, PARENT)
                .id();
            s.with_counter(other, charge(), 8);
            let oil_response = s
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic oil baseline response",
                    true,
                    "Put two oil counters on target creature.",
                )
                .id();
            let charge_response = s
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic charge baseline response",
                    true,
                    "Put two charge counters on target creature.",
                )
                .id();
            let mut r = s.build();
            if add {
                r = reload(r, true);
            }
            let mut committed = r.cast(source).commit();
            let mut stacked = false;
            for _ in 0..30 {
                if committed.state().objects[&source].zone == Zone::Battlefield
                    && committed
                        .state()
                        .stack
                        .iter()
                        .any(|entry| entry.source_id == source)
                {
                    stacked = true;
                    break;
                }
                assert!(matches!(
                    committed.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                committed.act(GameAction::PassPriority).unwrap();
            }
            assert!(stacked, "the public cast must stack its real ETB trigger");
            // CR 122.2: establish counters on the entered permanent through
            // public responses; counters on a hand card do not survive entry.
            for response in [Some(oil_response), (initial > 0).then_some(charge_response)]
                .into_iter()
                .flatten()
            {
                let mut response_commit = committed.cast(response).target_object(source).commit();
                for _ in 0..20 {
                    if response_commit.state().objects[&response].zone == Zone::Graveyard {
                        break;
                    }
                    assert!(matches!(
                        response_commit.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    response_commit.act(GameAction::PassPriority).unwrap();
                }
                assert_eq!(
                    response_commit.state().objects[&response].zone,
                    Zone::Graveyard
                );
                assert!(response_commit
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == source));
            }
            assert_eq!(
                committed.state().objects[&source]
                    .counters
                    .get(&charge())
                    .copied()
                    .unwrap_or(0),
                initial
            );
            assert_eq!(
                committed.state().objects[&source]
                    .counters
                    .get(&oil())
                    .copied(),
                Some(2)
            );
            assert!(matches!(
                committed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let outcome = committed.resolve();
            outcome.assert_zone(&[source], Zone::Battlefield);
            assert!(outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseCounterAdjustment,
                    ..
                }
            )));
            prompt(&r, if initial == 0 { 1 } else { 2 });
            let index = if initial == 0 || !add { 0 } else { 1 };
            let events = choose(&mut r, index);
            assert_eq!(
                n(&r, source, &charge()),
                if add { initial + 1 } else { initial - 1 }
            );
            assert_eq!(n(&r, source, &oil()), 2);
            assert_eq!(n(&r, other, &charge()), 8);
            assert_eq!(event_count(&events, source, &charge(), add), 1);
        }
    }
}
#[test]
fn public_nonself_trigger_adjusts_its_event_recipient_and_refuses_departed_twin() {
    // Synthetic class consumer. CR 608.2k + CR 400.7: "it" names the entering
    // creature, and a later zone change cannot substitute another incarnation.
    for departed in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let watcher = s
            .add_creature_from_oracle(
                P0,
                "Synthetic nonself watcher",
                2,
                4,
                "Whenever another creature enters, put a charge counter on it or remove one from it. You gain 1 life.",
            )
            .id();
        let decoy = s.add_creature(P0, "Same entrant", 2, 4).id();
        s.with_counter(watcher, charge(), 8)
            .with_counter(watcher, oil(), 3)
            .with_counter(decoy, charge(), 7);
        let entrant = s
            .add_creature_to_hand_from_oracle(P0, "Same entrant", 2, 4, "")
            .id();
        let response = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic event recipient departure",
                true,
                "Destroy target creature.",
            )
            .id();
        let mut r = s.build();
        let mut committed = r.cast(entrant).commit();
        let mut stacked = false;
        for _ in 0..30 {
            if committed.state().objects[&entrant].zone == Zone::Battlefield
                && committed
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == watcher)
            {
                stacked = true;
                break;
            }
            assert!(matches!(
                committed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            committed.act(GameAction::PassPriority).unwrap();
        }
        assert!(stacked, "the real nonself ETB trigger must reach the stack");
        let stamp = ObjectIncarnationRef::from_object(&committed.state().objects[&entrant]);
        let trigger = committed
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == watcher)
            .unwrap()
            .ability()
            .unwrap();
        assert!(
            trigger.targets.is_empty(),
            "event recipients declare no target slot"
        );
        assert!(matches!(
            trigger.effect,
            Effect::ChooseCounterAdjustment {
                target: TargetFilter::TriggeringSource,
                ..
            }
        ));
        if departed {
            let mut response_commit = committed.cast(response).target_object(entrant).commit();
            for _ in 0..20 {
                if response_commit.state().objects[&entrant].zone == Zone::Graveyard {
                    break;
                }
                assert!(matches!(
                    response_commit.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                response_commit.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(
                response_commit.state().objects[&entrant].zone,
                Zone::Graveyard
            );
            assert_ne!(
                ObjectIncarnationRef::from_object(&response_commit.state().objects[&entrant]),
                stamp
            );
            assert!(response_commit
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == watcher));
        }
        let outcome = committed.resolve();
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::ChooseCounterAdjustment, source_id, .. } if *source_id == watcher)));
        if departed {
            outcome.assert_zone(&[entrant], Zone::Graveyard);
            outcome.assert_life_delta(P0, 1);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert_eq!(event_count(outcome.events(), entrant, &charge(), true), 0);
            assert_eq!(n(&r, entrant, &charge()), 0);
            let mut events = vec![];
            finish(&mut r, &mut events);
            assert_eq!(r.life(P0), 21);
        } else {
            outcome.assert_zone(&[entrant], Zone::Battlefield);
            outcome.assert_life_delta(P0, 0);
            prompt(&r, 1);
            let WaitingFor::ChooseOneOfBranch {
                parent_occurrences, ..
            } = &r.state().waiting_for
            else {
                unreachable!()
            };
            assert_eq!(
                parent_occurrences.occurrences(),
                vec![(
                    engine::types::ability::TargetRef::Object(entrant),
                    Some(stamp)
                )]
            );
            r = reload(r, true);
            let events = choose(&mut r, 0);
            assert_eq!(n(&r, entrant, &charge()), 1);
            assert_eq!(event_count(&events, entrant, &charge(), true), 1);
            assert_eq!(event_count(&events, entrant, &charge(), false), 0);
            assert_eq!(r.life(P0), 21);
        }
        assert_eq!(n(&r, watcher, &charge()), 8);
        assert_eq!(n(&r, watcher, &oil()), 3);
        assert_eq!(n(&r, decoy, &charge()), 7);
    }
}
#[test]
fn public_damage_trigger_adjustment_uses_event_target_instead_of_dealer() {
    // Synthetic passive-voice class. CR 608.2k: the damaged creature is "it".
    for index in [0, 1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let watcher = s.add_creature_from_oracle(
            P0, "Synthetic damage watcher", 2, 4,
            "Whenever a creature is dealt damage, put a charge counter on it or remove one from it. You gain 1 life.",
        ).id();
        let recipient = s.add_creature(P1, "Damage recipient", 2, 4).id();
        let decoy = s.add_creature(P0, "Other recipient", 2, 4).id();
        s.with_counter(recipient, charge(), 2)
            .with_counter(recipient, oil(), 2)
            .with_counter(watcher, charge(), 8)
            .with_counter(decoy, charge(), 7);
        let spell = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic damage dealer",
                true,
                "Deal 1 damage to target creature.",
            )
            .id();
        let mut r = s.build();
        let outcome = r.cast(spell).target_object(recipient).resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, 0);
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: engine::types::ability::TargetRef::Object(id), amount: 1, .. } if *source_id == spell && *id == recipient)));
        prompt(&r, 2);
        let WaitingFor::ChooseOneOfBranch {
            parent_occurrences, ..
        } = &r.state().waiting_for
        else {
            unreachable!()
        };
        assert_eq!(
            parent_occurrences.occurrences(),
            vec![(
                engine::types::ability::TargetRef::Object(recipient),
                Some(ObjectIncarnationRef::from_object(
                    &r.state().objects[&recipient]
                ))
            )]
        );
        r = reload(r, true);
        let events = choose(&mut r, index);
        assert_eq!(n(&r, recipient, &charge()), if index == 0 { 1 } else { 3 });
        assert_eq!(event_count(&events, recipient, &charge(), index == 1), 1);
        assert_eq!(event_count(&events, spell, &charge(), true), 0);
        assert_eq!(n(&r, recipient, &oil()), 2);
        assert_eq!(n(&r, watcher, &charge()), 8);
        assert_eq!(n(&r, decoy, &charge()), 7);
        assert_eq!(n(&r, spell, &charge()), 0);
        assert_eq!(r.life(P0), 21);
    }
}
#[test]
fn loaded_bound_context_adjustment_delivers_and_keeps_pin_after_pause() {
    // Synthetic loaded-definition consumer of the ordinary context-filter reader.
    // CR 400.7: a new incarnation after the actual prompt cannot receive delivery.
    for stale in [false, true] {
        for index in [0, 1] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let recipient = s.add_creature(P0, "Bound recipient", 2, 4).id();
            let decoy = s.add_creature(P0, "Bound context decoy", 2, 4).id();
            s.with_counter(recipient, charge(), 2)
                .with_counter(recipient, oil(), 2)
                .with_counter(decoy, charge(), 8);
            let mut def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseCounterAdjustment {
                    adjustment: CounterAdjustment::AddOrRemove,
                    counters: CounterMatch::OfType(charge()),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SpecificObject { id: recipient },
                },
            );
            def.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )));
            let def: AbilityDefinition =
                serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
            let spell = s
                .add_spell_to_hand(P0, "Synthetic bound context", true)
                .with_ability_definition(def)
                .id();
            let mut r = s.build();
            let outcome = r.cast(spell).resolve();
            outcome.assert_zone(&[spell], Zone::Graveyard);
            outcome.assert_life_delta(P0, 0);
            prompt(&r, 2);
            let pin = ObjectIncarnationRef::from_object(&r.state().objects[&recipient]);
            let WaitingFor::ChooseOneOfBranch {
                parent_occurrences, ..
            } = &r.state().waiting_for
            else {
                unreachable!()
            };
            assert_eq!(
                parent_occurrences.occurrences(),
                vec![(
                    engine::types::ability::TargetRef::Object(recipient),
                    Some(pin)
                )]
            );
            r = reload(r, true);
            if stale {
                // Marked hostile mutation: no legal priority action exists in this pause.
                r.state_mut()
                    .objects
                    .get_mut(&recipient)
                    .unwrap()
                    .incarnation += 1;
                assert_ne!(
                    ObjectIncarnationRef::from_object(&r.state().objects[&recipient]),
                    pin
                );
            }
            let events = choose(&mut r, index);
            assert_eq!(
                n(&r, recipient, &charge()),
                if stale {
                    2
                } else if index == 0 {
                    1
                } else {
                    3
                }
            );
            assert_eq!(
                event_count(&events, recipient, &charge(), index == 1),
                if stale { 0 } else { 1 }
            );
            assert_eq!(n(&r, recipient, &oil()), 2);
            assert_eq!(n(&r, decoy, &charge()), 8);
            assert_eq!(r.life(P0), 21);
        }
    }
}
#[test]
fn actual_prompt_occurrence_survives_reload_and_refuses_new_incarnation() {
    for stale in [false, true] {
        for index in [0, 1] {
            let (mut r, id, decoy) = public_spell(THREE_TEXT, None, 5, (false, false));
            prompt(&r, 2);
            let pin = ObjectIncarnationRef::from_object(&r.state().objects[&id]);
            let serialized = serde_json::to_string(&r.state().waiting_for).unwrap();
            assert!(serialized.contains("incarnation"));
            r = reload(r, true);
            if stale {
                r.state_mut().objects.get_mut(&id).unwrap().incarnation += 1;
                assert_ne!(
                    ObjectIncarnationRef::from_object(&r.state().objects[&id]),
                    pin
                );
            }
            let events = choose(&mut r, index);
            assert_eq!(
                n(&r, id, &charge()),
                if stale {
                    5
                } else if index == 0 {
                    2
                } else {
                    8
                }
            );
            assert_eq!(n(&r, decoy, &charge()), 8);
            assert_eq!(
                event_count(&events, id, &charge(), index == 1),
                if stale { 0 } else { 3 }
            );
            assert_eq!(r.life(P0), 21);
        }
    }
}
#[test]
fn mandatory_prompt_validates_actor_index_and_keeps_tail_private() {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    let (mut r, id, _) = public_spell(THREE_TEXT, None, 5, (false, false));
    prompt(&r, 2);
    let before = r.state().clone();
    assert!(
        engine::game::engine::apply(r.state_mut(), P1, GameAction::ChooseBranch { index: 0 })
            .is_err()
    );
    assert_eq!(*r.state(), before);
    assert!(r.act(GameAction::ChooseBranch { index: 99 }).is_err());
    assert_eq!(n(&r, id, &charge()), 5);
    assert_eq!(r.life(P0), 20);
    assert!(
        !r.state().resolution_stack.is_empty(),
        "trusted state retains paused tail authority"
    );
    for public in [
        filter_state_for_viewer(r.state(), P0),
        filter_state_for_viewer(r.state(), P1),
        filter_state_for_unseated_viewer(r.state()),
    ] {
        let value = serde_json::to_value(ClientGameStateRef::wrap(&public, None)).unwrap();
        assert!(value.to_string().contains("remove 3"));
        assert!(public.resolution_stack.is_empty());
        let WaitingFor::ChooseOneOfBranch { continuation, .. } = &public.waiting_for else {
            panic!("public prompt")
        };
        assert!(continuation.is_none());
    }
    let events = choose(&mut r, 0);
    assert_eq!(n(&r, id, &charge()), 2);
    assert_eq!(event_count(&events, id, &charge(), false), 3);
    assert_eq!(r.life(P0), 21);
}
#[test]
fn legacy_anaphoric_axes_cast_and_deliver_both_present_kind_operations() {
    // Synthetic legacy grammar-axis fixtures, alongside the full pinned card.
    // CR 122.1 + CR 608.2d: choose one present kind and one adjustment at resolution.
    // CR 608.2c: the independent life instruction waits for that choice and runs once.
    for from in [
        "",
        " from it",
        " from that permanent",
        " from that card",
        " from that permanent or card",
    ] {
        for on in [
            "it",
            "that permanent",
            "that card",
            "that permanent or card",
        ] {
            let text = format!(
                "Choose a counter on target permanent. Remove that counter{from} or put another of those counters on {on}. You gain 1 life."
            );
            for (index, expected) in [(2, 2), (3, 4)] {
                let mut s = GameScenario::new();
                s.at_phase(Phase::PreCombatMain);
                mana(&mut s);
                let id = s.add_creature(P0, "Legacy recipient", 2, 4).id();
                let decoy = s.add_creature(P1, "Other legal recipient", 2, 4).id();
                s.with_counter(id, CounterType::Time, 3)
                    .with_counter(id, oil(), 2)
                    .with_counter(decoy, CounterType::Time, 8);
                let spell = s
                    .add_spell_to_hand_from_oracle(P0, "Synthetic legacy axes", true, &text)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                let mut r = s.build();
                let outcome = r.cast(spell).target_object(id).resolve();
                outcome.assert_zone(&[spell], Zone::Graveyard);
                outcome.assert_life_delta(P0, 0);
                prompt(&r, 4);
                assert_eq!(n(&r, id, &CounterType::Time), 3);
                assert_eq!(n(&r, id, &oil()), 2);
                r = reload(r, index == 3);
                prompt(&r, 4);
                let events = choose(&mut r, index);
                assert_eq!(n(&r, id, &CounterType::Time), expected, "{text}");
                assert_eq!(event_count(&events, id, &CounterType::Time, index == 3), 1);
                assert_eq!(event_count(&events, id, &CounterType::Time, index != 3), 0);
                assert_eq!(n(&r, id, &oil()), 2);
                assert_eq!(event_count(&events, id, &oil(), true), 0);
                assert_eq!(event_count(&events, id, &oil(), false), 0);
                assert_eq!(n(&r, decoy, &CounterType::Time), 8);
                assert_eq!(event_count(&events, decoy, &CounterType::Time, true), 0);
                assert_eq!(event_count(&events, decoy, &CounterType::Time, false), 0);
                assert_eq!(r.life(P0), 21);
            }
        }
    }
}

#[test]
fn clockspinning_complete_oracle_live_present_kind_and_empty_controls() {
    for (initial, index, expected) in [(3, 0, 2), (3, 1, 4), (0, 0, 0)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        mana(&mut s);
        let id = s.add_creature(P0, "Clock target", 2, 4).id();
        s.with_counter(id, CounterType::Time, initial);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Clockspinning", true, CLOCK)
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        let mut r = s.build();
        let outcome = r.cast(spell).target_object(id).resolve();
        outcome.assert_zone(&[spell], Zone::Graveyard);
        if initial == 0 {
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseCounterAdjustment,
                    ..
                }
            )));
        } else {
            prompt(&r, 2);
            r = reload(r, index == 1);
            let events = choose(&mut r, index);
            assert_eq!(event_count(&events, id, &CounterType::Time, index == 1), 1);
        }
        assert_eq!(n(&r, id, &CounterType::Time), expected);
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let id = s.add_creature(P0, "Two kinds", 2, 4).id();
    s.with_counter(id, charge(), 2).with_counter(id, oil(), 1);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Clockspinning", true, CLOCK)
        .id();
    let mut r = s.build();
    r.cast(spell).target_object(id).resolve();
    prompt(&r, 4);
    let events = choose(&mut r, 2);
    assert_eq!(n(&r, id, &oil()), 0);
    assert_eq!(n(&r, id, &charge()), 2);
    assert_eq!(event_count(&events, id, &oil(), false), 1);
}
#[test]
fn ordinary_replacement_delivery_changes_actual_count_and_pauses_without_preview_mutation() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let id = s.add_creature(P0, "Receiver", 2, 4).id();
    s.add_creature(P0, "Synthetic doubler", 2, 4)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::DOUBLE),
        );
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic one",
            true,
            "Put a charge counter on target creature or remove one from it. You gain 1 life.",
        )
        .id();
    let mut r = s.build();
    r.cast(spell).target_object(id).resolve();
    prompt(&r, 1);
    assert_eq!(n(&r, id, &charge()), 0);
    assert_eq!(r.life(P0), 20);
    let events = choose(&mut r, 0);
    assert_eq!(n(&r, id, &charge()), 2);
    assert_eq!(event_count(&events, id, &charge(), true), 2);
    assert_eq!(r.life(P0), 21);
}

#[test]
fn both_positive_operations_prohibited_complete_tail_without_prompt() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let id = s.add_creature(P0, "Blocked recipient", 2, 4).id();
    s.with_counter(id, charge(), 5).with_counter(id, oil(), 2);
    block(&mut s, id, true, true);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Synthetic blocked adjustment", true, THREE_TEXT)
        .id();
    let mut r = s.build();
    let out = r.cast(spell).target_object(id).resolve();
    assert!(matches!(
        out.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    out.assert_life_delta(P0, 1);
    assert!(out.events().iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::ChooseCounterAdjustment,
            ..
        }
    )));
    assert_eq!(event_count(out.events(), id, &charge(), true), 0);
    assert_eq!(event_count(out.events(), id, &charge(), false), 0);
    assert_eq!(n(&r, id, &charge()), 5);
    assert_eq!(n(&r, id, &oil()), 2);
}
#[test]
fn legacy_loaded_any_defaults_execute_present_kind_and_empty_population() {
    for (present, zero) in [(false, false), (false, true), (true, false)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let id = s.add_creature(P0, "Legacy target", 2, 4).id();
        if present {
            s.with_counter(id, CounterType::Time, 3);
        }
        let target = engine::parser::parse_oracle_text(
            "Put a charge counter on target creature.",
            "Synthetic",
            &[],
            &["Instant".into()],
            &[],
        )
        .abilities[0]
            .effect
            .target_filter()
            .unwrap()
            .clone();
        let mut raw =
            serde_json::json!({"type":"ChooseCounterAdjustment","adjustment":"AddOrRemove"});
        if zero {
            raw["count"] = serde_json::json!({"type":"Fixed","value":0});
        }
        let legacy: Effect = serde_json::from_value(raw).unwrap();
        let mut head = AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target });
        let mut sub = AbilityDefinition::new(AbilityKind::Spell, legacy);
        sub.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )));
        head.sub_ability = Some(Box::new(sub));
        let spell = s
            .add_spell_to_hand(P0, "Synthetic legacy loaded consumer", true)
            .with_ability_definition(head)
            .id();
        let mut r = s.build();
        let out = r.cast(spell).target_object(id).resolve();
        if present {
            out.assert_life_delta(P0, 0);
            prompt(&r, 2);
            r = reload(r, true);
            let events = choose(&mut r, 0);
            assert_eq!(n(&r, id, &CounterType::Time), 2);
            assert_eq!(event_count(&events, id, &CounterType::Time, false), 1);
            assert_eq!(r.life(P0), 21);
        } else {
            out.assert_life_delta(P0, 1);
            assert!(matches!(
                out.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert!(out.events().iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseCounterAdjustment,
                    ..
                }
            )));
        }
    }
    // Zero cannot invent charge for Any's separately required present-kind choice.
    for index in [0, 1] {
        let (mut r, id, _) = loaded_spell(
            QuantityExpr::Fixed { value: 0 },
            0,
            2,
            CounterMatch::Any,
            (false, false),
        );
        prompt(&r, 2);
        if let WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            ..
        } = &r.state().waiting_for
        {
            assert!(branch_descriptions
                .iter()
                .all(|label| label.contains("oil") && !label.contains("charge")));
        }
        r = reload(r, true);
        let events = choose(&mut r, index);
        assert_eq!(n(&r, id, &charge()), 0);
        assert_eq!(n(&r, id, &oil()), 2);
        assert_eq!(event_count(&events, id, &oil(), true), 0);
        assert_eq!(event_count(&events, id, &oil(), false), 0);
        assert_eq!(r.life(P0), 21);
    }
}
#[test]
fn selected_addition_replacement_order_pauses_then_delivers_actual_event_and_tail_once() {
    for order in [0, 1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let id = s.add_creature(P0, "Receiver", 2, 4).id();
        s.add_creature(P0, "Synthetic Double", 2, 4)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::DOUBLE)
                    .description("Double".into()),
            );
        s.add_creature(P0, "Synthetic Plus", 2, 4)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Plus { value: 1 })
                    .description("Plus".into()),
            );
        let spell = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic replacement adjustment",
                true,
                "Put a charge counter on target creature or remove one from it. You gain 1 life.",
            )
            .id();
        let mut r = s.build();
        r.cast(spell).target_object(id).resolve();
        prompt(&r, 1);
        assert_eq!(n(&r, id, &charge()), 0);
        assert!(r.state().pending_replacement.is_none());
        let mut events = r.act(GameAction::ChooseBranch { index: 0 }).unwrap().events;
        let WaitingFor::ReplacementChoice {
            candidate_count,
            candidates,
            ..
        } = &r.state().waiting_for
        else {
            panic!("ordinary addition must reach replacement ordering")
        };
        assert_eq!(*candidate_count, 2);
        let expected = if candidates[order].source_name == "Synthetic Double" {
            3
        } else {
            assert_eq!(candidates[order].source_name, "Synthetic Plus");
            4
        };
        assert_eq!(n(&r, id, &charge()), 0);
        assert_eq!(r.life(P0), 20);
        r = reload(r, true);
        events.extend(
            r.act(GameAction::ChooseReplacement { index: order })
                .unwrap()
                .events,
        );
        finish(&mut r, &mut events);
        let actual = n(&r, id, &charge());
        assert_eq!(
            actual, expected,
            "double then plus gives3, plus then double gives4"
        );
        assert_eq!(event_count(&events, id, &charge(), true), actual);
        assert_eq!(r.life(P0), 21);
    }
}
#[test]
fn source_removed_in_public_response_cannot_receive_or_substitute_counters() {
    for removed in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let other = s.add_creature(P0, "Immard, the Stormcleaver", 4, 4).id();
        s.with_counter(other, charge(), 8);
        let text = format!("{PARENT} You gain 1 life.");
        let source = s
            .add_creature_to_hand_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, &text)
            .id();
        let response = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic destroy response",
                true,
                "Destroy target creature.",
            )
            .id();
        let mut r = s.build();
        let mut committed = r.cast(source).commit();
        let mut stacked = false;
        for _ in 0..30 {
            if committed.state().objects[&source].zone == Zone::Battlefield
                && committed
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == source)
            {
                stacked = true;
                break;
            }
            assert!(matches!(
                committed.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            committed.act(GameAction::PassPriority).unwrap();
        }
        assert!(
            stacked,
            "real ETB trigger must be on stack before source response"
        );
        let stamp = ObjectIncarnationRef::from_object(&committed.state().objects[&source]);
        if removed {
            // Cast the response, then resolve only it via public priority actions.
            let mut response_commit = committed.cast(response).target_object(source).commit();
            for _ in 0..20 {
                if response_commit.state().objects[&source].zone == Zone::Graveyard {
                    break;
                }
                response_commit.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(
                response_commit.state().objects[&source].zone,
                Zone::Graveyard
            );
            assert_ne!(
                ObjectIncarnationRef::from_object(&response_commit.state().objects[&source]),
                stamp
            );
        }
        let out = committed.resolve();
        if removed {
            out.assert_life_delta(P0, 1);
            assert!(matches!(
                out.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert_eq!(n(&r, source, &charge()), 0);
        } else {
            prompt(&r, 1);
            let events = choose(&mut r, 0);
            assert_eq!(event_count(&events, source, &charge(), true), 1);
            assert_eq!(n(&r, source, &charge()), 1);
            assert_eq!(r.life(P0), 21);
        }
        assert_eq!(n(&r, other, &charge()), 8);
    }
}
#[test]
fn public_attack_trigger_reaches_mandatory_source_choice() {
    use engine::game::combat::AttackTarget;
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = s
        .add_creature_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, PARENT)
        .haste()
        .id();
    let other = s.add_creature(P1, "Immard, the Stormcleaver", 4, 4).id();
    s.with_counter(source, charge(), 2)
        .with_counter(other, charge(), 8);
    let mut r = s.build();
    r.advance_to_phase(Phase::DeclareAttackers);
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .unwrap();
    let mut reached = false;
    for _ in 0..30 {
        match r.state().waiting_for {
            WaitingFor::ChooseOneOfBranch { .. } => {
                reached = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("{other:?}"),
        }
    }
    assert!(reached);
    prompt(&r, 2);
    let events = choose(&mut r, 0);
    assert_eq!(n(&r, source, &charge()), 1);
    assert_eq!(event_count(&events, source, &charge(), false), 1);
    assert_eq!(n(&r, other, &charge()), 8);
}

#[test]
fn actual_removal_prevented_by_replacement_emits_no_intent_removal() {
    for prevent in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let id = s.add_creature(P0, "Removal receiver", 2, 4).id();
        s.with_counter(id, charge(), 2).with_counter(id, oil(), 3);
        if prevent {
            s.add_creature(P0, "Synthetic removal replacement", 2, 4)
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                        .valid_card(TargetFilter::SpecificObject { id })
                        .quantity_modification(QuantityModification::Prevent),
                );
        }
        let spell = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic removal event",
                true,
                "Put a charge counter on target creature or remove one from it. You gain 1 life.",
            )
            .id();
        let mut r = s.build();
        r.cast(spell).target_object(id).resolve();
        prompt(&r, 2);
        let events = choose(&mut r, 0);
        assert_eq!(n(&r, id, &charge()), if prevent { 2 } else { 1 });
        assert_eq!(
            event_count(&events, id, &charge(), false),
            if prevent { 0 } else { 1 }
        );
        assert_eq!(n(&r, id, &oil()), 3);
        assert_eq!(r.life(P0), 21);
    }
}
#[test]
fn clockspinning_suspended_target_legality_and_live_kind_population() {
    use engine::types::keywords::Keyword;
    for suspended in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        mana(&mut s);
        let mut b = s.add_creature_to_exile(P0, "Exiled counter card", 2, 4);
        if suspended {
            b.with_keyword(Keyword::Suspend {
                count: 3,
                cost: ManaCost::zero(),
            });
        }
        let id = b.id();
        s.with_counter(id, CounterType::Time, 3);
        let legal = s.add_creature(P0, "Legal positive target", 2, 4).id();
        s.with_counter(legal, CounterType::Time, 3);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Clockspinning", true, CLOCK)
            .id();
        let mut r = s.build();
        if suspended {
            r.cast(spell).target_object(id).resolve();
            prompt(&r, 2);
            let events = choose(&mut r, 0);
            assert_eq!(n(&r, id, &CounterType::Time), 2);
            assert_eq!(event_count(&events, id, &CounterType::Time, false), 1);
        } else {
            let parsed = engine::parser::parse_oracle_text(
                CLOCK,
                "Clockspinning",
                &[],
                &["Instant".into()],
                &[],
            );
            let targets = engine::game::targeting::find_legal_targets(
                r.state(),
                parsed.abilities[0].effect.target_filter().unwrap(),
                P0,
                spell,
            );
            assert!(!targets.contains(&engine::types::ability::TargetRef::Object(id)));
            assert!(
                targets.contains(&engine::types::ability::TargetRef::Object(legal)),
                "positive target reaches the same announced filter"
            );
            r.cast(spell).target_object(legal).resolve();
            prompt(&r, 2);
            choose(&mut r, 0);
            assert_eq!(n(&r, legal, &CounterType::Time), 2);
            assert_eq!(n(&r, id, &CounterType::Time), 3);
        }
    }
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let id = s.add_creature(P0, "Live kind target", 2, 4).id();
    s.with_counter(id, CounterType::Time, 3);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Clockspinning", true, CLOCK)
        .id();
    let response = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic counter response",
            true,
            "Remove all time counters from target permanent. Put an oil counter on it.",
        )
        .id();
    let mut r = s.build();
    let mut committed = r.cast(spell).target_object(id).commit();
    {
        let mut response = committed.cast(response).target_object(id).commit();
        for _ in 0..20 {
            if response.state().objects[&response.state().stack.last().unwrap().source_id].name
                == "Clockspinning"
            {
                break;
            }
            response.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(
            response.state().objects[&id]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            0
        );
        assert_eq!(
            response.state().objects[&id]
                .counters
                .get(&oil())
                .copied()
                .unwrap_or(0),
            1
        );
    }
    committed.resolve();
    prompt(&r, 2);
    if let WaitingFor::ChooseOneOfBranch {
        branch_descriptions,
        ..
    } = &r.state().waiting_for
    {
        assert!(branch_descriptions
            .iter()
            .all(|label| label.contains("oil")));
    }
    let events = choose(&mut r, 0);
    assert_eq!(n(&r, id, &oil()), 0);
    assert_eq!(event_count(&events, id, &oil(), false), 1);
}
#[test]
fn announced_clockspinning_target_blink_refuses_new_incarnation_without_substitution() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let id = s.add_creature(P0, "Blink target", 2, 4).id();
    let decoy = s.add_creature(P0, "Other legal target", 2, 4).id();
    s.with_counter(id, CounterType::Time, 3)
        .with_counter(decoy, CounterType::Time, 7);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Clockspinning", true, CLOCK)
        .id();
    let response = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic blink response",
            true,
            "Exile target creature, then return it to the battlefield under its owner's control.",
        )
        .id();
    let mut r = s.build();
    let mut committed = r.cast(spell).target_object(id).commit();
    let stamp = ObjectIncarnationRef::from_object(&committed.state().objects[&id]);
    {
        let mut response = committed.cast(response).target_object(id).commit();
        for _ in 0..20 {
            if response.state().objects[&id].incarnation != stamp.incarnation
                && response.state().objects[&id].zone == Zone::Battlefield
            {
                break;
            }
            response.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(response.state().objects[&id].zone, Zone::Battlefield);
        assert_ne!(
            ObjectIncarnationRef::from_object(&response.state().objects[&id]),
            stamp
        );
    }
    let out = committed.resolve();
    assert!(matches!(
        out.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(n(&r, id, &CounterType::Time), 0);
    assert_eq!(n(&r, decoy, &CounterType::Time), 7);
}

#[test]
fn optional_adjustment_shell_keeps_its_ordinary_decline() {
    let text =
        "You may put a charge counter on target creature or remove one from it. You gain 1 life.";
    for accept in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let id = s.add_creature(P0, "Optional recipient", 2, 4).id();
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Synthetic optional sibling", true, text)
            .id();
        let mut r = s.build();
        if accept {
            r.cast(spell).target_object(id).accept_optional().resolve();
            prompt(&r, 1);
            choose(&mut r, 0);
            assert_eq!(n(&r, id, &charge()), 1);
        } else {
            let out = r.cast(spell).target_object(id).decline_optional().resolve();
            out.assert_life_delta(P0, 1);
            assert!(matches!(
                out.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert_eq!(n(&r, id, &charge()), 0);
        }
        assert_eq!(r.life(P0), 21);
    }
}
#[test]
fn source_controller_change_keeps_captured_chooser_and_exact_recipient() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let source = s
        .add_creature_to_hand_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, PARENT)
        .id();
    let other = s.add_creature(P1, "Other controller", 4, 4).id();
    s.with_counter(other, charge(), 8);
    let response = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic control exchange",
            true,
            "Exchange control of two target creatures.",
        )
        .id();
    let mut r = s.build();
    let mut committed = r.cast(source).commit();
    let mut stacked = false;
    for _ in 0..30 {
        if committed.state().objects[&source].zone == Zone::Battlefield
            && committed
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == source)
        {
            stacked = true;
            break;
        }
        committed.act(GameAction::PassPriority).unwrap();
    }
    assert!(stacked);
    let stamp = ObjectIncarnationRef::from_object(&committed.state().objects[&source]);
    {
        let mut response = committed
            .cast(response)
            .target_objects(&[source, other])
            .commit();
        for _ in 0..20 {
            if response.state().objects[&source].controller == P1 {
                break;
            }
            response.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(response.state().objects[&source].controller, P1);
        assert_eq!(
            ObjectIncarnationRef::from_object(&response.state().objects[&source]),
            stamp
        );
    }
    committed.resolve();
    prompt(&r, 1);
    let events = choose(&mut r, 0);
    assert_eq!(n(&r, source, &charge()), 1);
    assert_eq!(n(&r, other, &charge()), 8);
    assert!(events.iter().any(|e|matches!(e,GameEvent::CounterAdded { object_id,actor,count:1,.. } if *object_id==source&&*actor==P0)));
}

#[test]
fn u13_public_hostile_removals_and_zone_clearing_cannot_create_an_add_child() {
    for hostile in 0..4 {
        let other = std::cell::Cell::new(ObjectId(0));
        let response = std::cell::Cell::new(ObjectId(0));
        let mut f = u13_fixture_with_responses(
            IMMARD,
            1,
            |s, _, _| {
                other.set(s.add_creature(P0, "Immard, the Stormcleaver", 4, 4).id());
                s.with_counter(other.get(), if hostile == 2 { oil() } else { charge() }, 2);
                response.set(s.add_spell_to_hand_from_oracle(P0, "Synthetic hostile provenance response", true,
                if hostile < 2 { CLOCK } else if hostile == 2 { "Remove an oil counter from target permanent." }
                else { "Exile target creature. Return it to the battlefield under its owner's control." }).id());
            },
            |r, source| {
                if hostile < 2 {
                    let target = if hostile == 0 { source } else { other.get() };
                    r.cast(response.get()).target_object(target).resolve();
                    prompt(r, 2);
                    let out = choose(r, 0);
                    assert_eq!(event_count(&out, target, &charge(), false), 1);
                } else {
                    let old = ObjectIncarnationRef::from_object(&r.state().objects[&other.get()]);
                    u13_response(r, response.get(), other.get(), None);
                    if hostile == 2 {
                        assert_eq!(n(r, other.get(), &oil()), 1);
                    } else {
                        assert_ne!(
                            old,
                            ObjectIncarnationRef::from_object(&r.state().objects[&other.get()])
                        );
                        assert_eq!(n(r, other.get(), &charge()), 0);
                    }
                }
            },
        );
        let before = n(&f.runner, f.source, &charge());
        prompt(&f.runner, if before == 0 { 1 } else { 2 });
        f.runner
            .act(GameAction::ChooseBranch {
                index: if before == 0 { 0 } else { 1 },
            })
            .unwrap();
        finish(&mut f.runner, &mut Vec::new());
        assert_eq!(n(&f.runner, f.source, &charge()), before + 1);
        assert_eq!(f.runner.life(P2), 20);
        assert!(f.runner.state().return_result_frames.is_empty());
        let mut twin = u13_fixture(IMMARD, 1, |_, _, _| {});
        u13_remove_and_announce(&mut twin.runner, twin.source, 0, P2);
        finish(&mut twin.runner, &mut Vec::new());
        assert_eq!(twin.runner.life(P2), 16);
    }
}

#[test]
fn u13_public_new_parent_after_countered_child_earns_only_its_new_child() {
    let mut refill = ObjectId(0);
    let mut f = u13_fixture(IMMARD, 1, |s, _, _| {
        refill = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic later stock",
                true,
                "Put a charge counter on target creature.",
            )
            .id();
    });
    let first = u13_remove_and_announce(&mut f.runner, f.source, 0, P2);
    f.runner.cast(f.counter).target_object(first).resolve();
    finish(&mut f.runner, &mut Vec::new());
    assert_eq!(f.runner.life(P2), 20);
    assert!(f.runner.state().return_result_frames.is_empty());
    u13_response(&mut f.runner, refill, f.source, None);
    u13_response(&mut f.runner, f.haste, f.source, None);
    f.runner.advance_to_phase(Phase::DeclareAttackers);
    f.runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(f.source, engine::game::combat::AttackTarget::Player(P2))],
            bands: vec![],
        })
        .unwrap();
    u13_until(&mut f.runner, |state| {
        matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. })
    });
    let second = u13_remove_and_announce(&mut f.runner, f.source, 0, P2);
    assert_ne!(first, second);
    finish(&mut f.runner, &mut Vec::new());
    assert_eq!(f.runner.life(P2), 16);
    assert_eq!(n(&f.runner, f.source, &charge()), 0);
    assert!(f.runner.state().return_result_frames.is_empty());
}

#[test]
fn u13_public_target_legality_and_targetless_mode_guards() {
    for illegal in [false, true] {
        let mut victim = ObjectId(0);
        let mut exile = ObjectId(0);
        let mut f = u13_fixture(IMMARD, 1, |s, _, _| {
            victim = s.add_creature(P1, "Synthetic damage target", 2, 6).id();
            exile = s
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic target response",
                    true,
                    "Exile target creature.",
                )
                .id();
        });
        choose(&mut f.runner, 0);
        u13_until(&mut f.runner, |state| {
            matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
        });
        f.runner
            .act(GameAction::SelectModes { indices: vec![0] })
            .unwrap();
        assert!(f
            .runner
            .act(GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(ObjectId(
                    u64::MAX
                )))
            })
            .is_err());
        let action = engine::ai_support::legal_actions(f.runner.state()).into_iter().find(|action|
            matches!(action, GameAction::ChooseTarget { target: Some(engine::types::ability::TargetRef::Object(id)) } if *id == victim)).unwrap();
        f.runner.act(action).unwrap();
        assert!(f
            .runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == f.source));
        if illegal {
            u13_response(&mut f.runner, exile, victim, None);
        }
        finish(&mut f.runner, &mut Vec::new());
        assert_eq!(
            f.runner.state().objects[&victim].damage_marked,
            if illegal { 0 } else { 4 }
        );
        assert_eq!(n(&f.runner, f.source, &charge()), 0);
    }
    for all_required in [false, true] {
        let text = if all_required {
            "When this creature enters, put a charge counter on it or remove one from it. When you remove a counter this way, choose one —\n• Destroy target land.\n• Tap target land."
        } else {
            "When this creature enters, put a charge counter on it or remove one from it. When you remove a counter this way, choose one —\n• Destroy target land.\n• You gain 2 life."
        };
        let mut f = u13_fixture(text, 1, |_, _, _| {});
        choose(&mut f.runner, 0);
        if all_required {
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(f.runner.life(P0), 20);
        } else {
            u13_until(&mut f.runner, |state| {
                matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
            });
            let WaitingFor::AbilityModeChoice {
                unavailable_modes, ..
            } = &f.runner.state().waiting_for
            else {
                unreachable!()
            };
            assert!(unavailable_modes.contains(&0));
            assert!(!unavailable_modes.contains(&1));
            assert!(f
                .runner
                .act(GameAction::SelectModes { indices: vec![0] })
                .is_err());
            f.runner
                .act(GameAction::SelectModes { indices: vec![1] })
                .unwrap();
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(f.runner.life(P0), 22);
        }
    }
}

#[test]
fn u13_public_ordinary_unbound_current_source_double_positive() {
    use engine::types::ability::DamageModification;
    for state_level in [false, true] {
        let mut s = GameScenario::new_n_player(3, 17);
        s.at_phase(Phase::PreCombatMain);
        let mut pending = Vec::new();
        u13_install_damage_modifier(
            &mut s,
            &mut pending,
            state_level,
            P0,
            "Synthetic ordinary Double",
            DamageModification::Double,
            Some(u13_control_filter(false)),
        );
        let spell = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic ordinary damage",
                true,
                "This spell deals 4 damage to any target.",
            )
            .id();
        let mut r = s.build();
        r.state_mut().pending_damage_replacements.extend(pending);
        let outcome = r.cast(spell).target_player(P2).resolve();
        outcome.assert_life_delta(P2, -8);
        assert_eq!(r.life(P2), 12);
        assert_eq!(r.state().damage_dealt_this_turn.last().unwrap().amount, 8);
    }
}

#[test]
fn u13_authoritative_tickets_and_damage_carriers_are_private_at_every_projection() {
    for damage in [false, true] {
        let mut f = u13_fixture(IMMARD, if damage { 2 } else { 1 }, |s, _, pending| {
            if damage {
                for (name, modification) in [
                    (
                        "Synthetic privacy Minus",
                        engine::types::ability::DamageModification::Minus { value: 1 },
                    ),
                    (
                        "Synthetic privacy Double",
                        engine::types::ability::DamageModification::Double,
                    ),
                ] {
                    u13_install_damage_modifier(s, pending, false, P1, name, modification, None);
                }
            }
        });
        if damage {
            let child = u13_keyword_then_attack(&mut f);
            u13_change_source(&mut f, child, true, false);
            u13_until(&mut f.runner, |state| {
                matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
            });
            assert!(f
                .runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .damage_source_context
                .is_some());
        } else {
            let WaitingFor::ChooseOneOfBranch {
                context,
                branch_descriptions,
                ..
            } = &f.runner.state().waiting_for
            else {
                panic!("natural counter prompt")
            };
            assert!(context.counter_removal_result.is_some());
            assert_eq!(branch_descriptions.len(), 2);
            assert!(!f.runner.state().return_result_frames.is_empty());
        }
        for projected in [
            engine::game::visibility::filter_state_for_viewer(f.runner.state(), P0),
            engine::game::visibility::filter_state_for_viewer(f.runner.state(), P1),
            engine::game::visibility::filter_state_for_viewer(f.runner.state(), P2),
            engine::game::visibility::filter_state_for_unseated_viewer(f.runner.state()),
        ] {
            assert!(projected.return_result_frames.is_empty());
            assert!(projected.resolution_stack.is_empty());
            if let WaitingFor::ChooseOneOfBranch {
                context,
                continuation,
                branch_descriptions,
                ..
            } = &projected.waiting_for
            {
                assert!(context.counter_removal_result.is_none());
                assert!(continuation.is_none());
                assert_eq!(branch_descriptions.len(), 2);
            }
            if let Some(pending) = &projected.pending_replacement {
                assert!(pending.damage_source_context.is_none());
                assert!(!pending.candidates.is_empty());
            }
        }
        for viewer in [None, Some(P0), Some(P1), Some(P2)] {
            let client = serde_json::to_value(
                engine::game::derived_views::ClientGameStateRef::wrap(f.runner.state(), viewer),
            )
            .unwrap();
            assert!(client["state"]["waiting_for"]["context"]
                .get("counter_removal_result")
                .is_none());
            assert!(client["state"]["pending_replacement"]
                .get("damage_source_context")
                .is_none());
            assert!(client["state"].get("resolution_stack").is_none());
        }
        f.runner = reload(f.runner, true);
        if damage {
            f.runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap();
            finish(&mut f.runner, &mut Vec::new());
            assert!(
                f.runner
                    .state()
                    .damage_dealt_this_turn
                    .last()
                    .unwrap()
                    .amount
                    > 0
            );
        } else {
            u13_remove_and_announce(&mut f.runner, f.source, 0, P2);
            finish(&mut f.runner, &mut Vec::new());
            assert_eq!(f.runner.life(P2), 16);
        }
    }
}

#[test]
fn u13_current_damage_carrier_refusal_and_explicit_v7_exact_owner_reselection() {
    for (legacy, state_level) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut selected = Vec::new();
        let mut f = u13_fixture(IMMARD, 2, |s, _, pending| {
            for (name, modification, filter) in [
                (
                    "Synthetic wire Minus",
                    engine::types::ability::DamageModification::Minus { value: 1 },
                    None,
                ),
                (
                    "Synthetic wire B Double",
                    engine::types::ability::DamageModification::Double,
                    Some(u13_control_filter(false)),
                ),
                (
                    "Synthetic wire Plus",
                    engine::types::ability::DamageModification::Plus {
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    None,
                ),
            ] {
                selected.push(u13_install_damage_modifier(
                    s,
                    pending,
                    state_level,
                    P1,
                    name,
                    modification,
                    filter,
                ));
            }
        });
        let child = u13_keyword_then_attack(&mut f);
        let old = u13_change_source(&mut f, child, true, false);
        u13_until(&mut f.runner, |state| {
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
        });
        let good = serde_json::to_value(f.runner.state()).unwrap();
        for fault in 0..5 {
            let mut bad = good.clone();
            match fault {
                0 => {
                    bad["pending_replacement"]
                        .as_object_mut()
                        .unwrap()
                        .remove("damage_source_context");
                }
                1 => {
                    bad["pending_replacement"]["damage_source_context"]["identity"]["reference"]
                        ["object_id"] = serde_json::json!(u64::MAX)
                }
                2 => {
                    bad["pending_replacement"]["damage_source_context"]["identity"]["reference"]
                        ["incarnation"] = serde_json::json!(u64::MAX)
                }
                3 => {
                    bad["pending_replacement"]["damage_source_context"]["identity"]
                        ["expected_zone"] = serde_json::to_value(Zone::Hand).unwrap()
                }
                4 => {
                    bad["pending_replacement"]["proposed"] = serde_json::to_value(
                        engine::types::proposed_event::ProposedEvent::LifeGain {
                            player_id: P2,
                            amount: 4,
                            applied: Default::default(),
                        },
                    )
                    .unwrap()
                }
                _ => unreachable!(),
            }
            assert!(
                serde_json::from_value::<GameState>(bad).is_err(),
                "current fault {fault}"
            );
        }
        for chosen in [selected[0], selected[1]] {
            let mut wire = serde_json::to_value(ResolutionStateWire::from_game_state(
                f.runner.state().clone(),
            ))
            .unwrap();
            if legacy {
                wire["resolution_state_version"] = serde_json::json!(8);
                wire["pending_replacement"]
                    .as_object_mut()
                    .unwrap()
                    .remove("damage_source_context");
            }
            let restored: ResolutionStateWire = serde_json::from_value(wire).unwrap();
            f.runner = GameRunner::from_state(restored.into_game_state());
            let pending = f.runner.state().pending_replacement.as_ref().unwrap();
            assert_eq!(
                pending
                    .damage_source_context
                    .as_ref()
                    .unwrap()
                    .identity
                    .reference,
                old
            );
            assert!(pending.candidates.contains(&selected[1]));
            let index = pending
                .candidates
                .iter()
                .position(|id| *id == chosen)
                .unwrap();
            f.runner
                .act(GameAction::ChooseReplacement { index })
                .unwrap();
        }
        finish(&mut f.runner, &mut Vec::new());
        u13_damage_record(&f, old, 7, true);
    }
}

#[test]
fn u13_public_opposing_sources_use_ordinary_ordering_and_separate_receipts() {
    let text = "Whenever another creature enters, put a charge counter on this creature or remove one from it. When you remove a counter this way, choose one —\n• You gain 1 life.\n• You gain 2 life.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mut sources = vec![];
    for (player, name) in [
        (P0, "Synthetic A1"),
        (P0, "Synthetic A2"),
        (P1, "Synthetic B"),
    ] {
        let id = s
            .add_creature(player, name, 2, 4)
            .from_oracle_text(text)
            .id();
        s.with_counter(id, charge(), 1);
        sources.push(id);
    }
    let entrant = s
        .add_creature_to_hand(P0, "Synthetic co-trigger entrant", 2, 2)
        .id();
    let mut r = s.build();
    r.cast(entrant).commit();
    let mut parent_players = vec![];
    let mut mode_players = vec![];
    let mut ordered = false;
    for _ in 0..120 {
        match &r.state().waiting_for {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(*player, P0);
                assert_eq!(triggers.len(), 2);
                ordered = true;
                r.act(GameAction::OrderTriggers { order: vec![1, 0] })
                    .unwrap();
            }
            WaitingFor::ChooseOneOfBranch {
                player, source_id, ..
            } => {
                parent_players.push(*player);
                assert!(sources.contains(source_id));
                r.act(GameAction::ChooseBranch { index: 0 }).unwrap();
            }
            WaitingFor::AbilityModeChoice { player, .. } => {
                mode_players.push(*player);
                r.act(GameAction::SelectModes { indices: vec![0] }).unwrap();
            }
            WaitingFor::Priority { .. }
                if r.state().stack.is_empty() && parent_players.len() == 3 =>
            {
                break
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("ordinary APNAP route: {other:?}"),
        }
    }
    assert!(ordered);
    assert_eq!(parent_players, vec![P1, P0, P0]);
    assert_eq!(mode_players, vec![P1, P0, P0]);
    assert_eq!(r.life(P0), 22);
    assert_eq!(r.life(P1), 21);
    assert!(sources.iter().all(|source| n(&r, *source, &charge()) == 0));
    assert!(r.state().return_result_frames.is_empty());
}

#[test]
fn u13_public_all_operations_refused_has_no_child_with_a_full_card_positive_twin() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    mana(&mut s);
    let source = s
        .add_creature_to_hand_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, IMMARD)
        .id();
    block(&mut s, source, true, true);
    let mut r = s.build();
    let cast = r.cast(source).resolve();
    assert_eq!(cast.state().objects[&source].zone, Zone::Battlefield);
    let mut events = cast.events().to_vec();
    finish(&mut r, &mut events);
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::ChooseCounterAdjustment,
            ..
        }
    )));
    assert_eq!(n(&r, source, &charge()), 0);
    assert_eq!(r.life(P1), 20);
    let mut twin = u13_fixture(IMMARD, 1, |_, _, _| {});
    u13_remove_and_announce(&mut twin.runner, twin.source, 0, P2);
    finish(&mut twin.runner, &mut Vec::new());
    assert_eq!(twin.runner.life(P2), 16);
}

#[test]
fn u13_explicit_v7_ordinary_adjustment_and_damage_do_not_acquire_new_authority() {
    let mut f = u13_fixture(PARENT, 1, |_, _, _| {});
    let WaitingFor::ChooseOneOfBranch { context, .. } = &f.runner.state().waiting_for else {
        panic!("ordinary parent prompt")
    };
    assert!(context.counter_removal_result.is_none());
    assert!(f.runner.state().return_result_frames.is_empty());
    let mut old = serde_json::to_value(ResolutionStateWire::from_game_state(
        f.runner.state().clone(),
    ))
    .unwrap();
    old["resolution_state_version"] = serde_json::json!(8);
    let loaded: ResolutionStateWire = serde_json::from_value(old).unwrap();
    f.runner = GameRunner::from_state(loaded.into_game_state());
    choose(&mut f.runner, 0);
    finish(&mut f.runner, &mut Vec::new());
    assert_eq!(n(&f.runner, f.source, &charge()), 0);
    assert_eq!(f.runner.life(P2), 20);
    assert!(f.runner.state().return_result_frames.is_empty());

    let mut s = GameScenario::new_n_player(3, 23);
    s.at_phase(Phase::PreCombatMain);
    let mut pending = vec![];
    for (name, modification) in [
        (
            "Synthetic old Minus",
            engine::types::ability::DamageModification::Minus { value: 1 },
        ),
        (
            "Synthetic old Double",
            engine::types::ability::DamageModification::Double,
        ),
    ] {
        u13_install_damage_modifier(&mut s, &mut pending, false, P0, name, modification, None);
    }
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic old ordinary damage",
            true,
            "This spell deals 4 damage to any target.",
        )
        .id();
    let mut r = s.build();
    r.cast(spell).target_player(P2).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(r
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .damage_source_context
        .is_none());
    let mut old =
        serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone())).unwrap();
    old["resolution_state_version"] = serde_json::json!(8);
    old["pending_replacement"]
        .as_object_mut()
        .unwrap()
        .remove("damage_source_context");
    let restored: ResolutionStateWire = serde_json::from_value(old).unwrap();
    r = GameRunner::from_state(restored.into_game_state());
    assert!(r
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .damage_source_context
        .is_none());
    let minus = r
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .position(|id| r.state().objects[&id.source].name == "Synthetic old Minus")
        .unwrap();
    r.act(GameAction::ChooseReplacement { index: minus })
        .unwrap();
    finish(&mut r, &mut Vec::new());
    assert_eq!(r.life(P2), 14);
    assert_eq!(r.state().damage_dealt_this_turn.last().unwrap().amount, 6);
}

#[test]
fn u13_explicit_v7_captured_damage_with_tail_and_later_target_leg_resume() {
    use engine::types::ability::{DamageModification, DamageSource, TargetRef};
    use engine::types::proposed_event::ProposedEvent;

    for mixed in [false, true] {
        // Synthetic source-damage-plus-tail class; the mixed sibling's second
        // instruction deliberately has a different source-selection mechanism.
        let text = if mixed {
            "When this creature enters, it deals 1 damage to you. Target creature you control deals 2 damage to you. You gain 3 life."
        } else {
            "When this creature enters, it deals 1 damage to you. You gain 3 life."
        };
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature_to_hand_from_oracle(P0, "Synthetic legacy damage tail", 2, 4, text)
            .id();
        let mut pending = vec![];
        let mut selected = vec![];
        for (name, modification) in [
            (
                "Synthetic tail Plus",
                DamageModification::Plus {
                    value: QuantityExpr::Fixed { value: 1 },
                },
            ),
            ("Synthetic tail Double", DamageModification::Double),
            (
                "Synthetic tail last",
                DamageModification::Plus {
                    value: QuantityExpr::Fixed { value: 2 },
                },
            ),
        ] {
            selected.push(u13_install_damage_modifier(
                &mut s,
                &mut pending,
                false,
                P0,
                name,
                modification,
                None,
            ));
        }
        let mut r = s.build();
        r.cast(source).commit();
        for _ in 0..50 {
            match &r.state().waiting_for {
                WaitingFor::ReplacementChoice { .. } => break,
                WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                    assert!(mixed);
                    let action = engine::ai_support::legal_actions(r.state()).into_iter().find(|action|
                        matches!(action, GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) } if *id == source)).unwrap();
                    r.act(action).unwrap();
                }
                WaitingFor::Priority { .. } => {
                    r.act(GameAction::PassPriority).unwrap();
                }
                other => panic!("public source trigger route: {other:?}"),
            }
        }
        let mut events = vec![];
        for leg in 0..if mixed { 2 } else { 1 } {
            let owner = r
                .state()
                .resolving_stack_entry
                .as_ref()
                .unwrap()
                .ability()
                .unwrap();
            assert!(matches!(
                owner.effect,
                Effect::DealDamage {
                    damage_source: None,
                    ..
                }
            ));
            assert!(owner.trigger_source.is_some());
            assert!(owner.sub_ability.is_some());
            if leg == 0 {
                assert!(r
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .damage_source_context
                    .is_some());
            } else {
                // Same storage id as the captured root, but this event was
                // produced by Target, not by that root's own-source route.
                assert!(
                    std::iter::successors(owner.sub_ability.as_deref(), |node| node
                        .sub_ability
                        .as_deref())
                    .any(|node| matches!(
                        node.effect,
                        Effect::DealDamage {
                            damage_source: Some(DamageSource::Target),
                            ..
                        }
                    ))
                );
                assert!(r
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .damage_source_context
                    .is_none());
            }
            for (step, chosen) in [selected[0], selected[1]].into_iter().enumerate() {
                let pause = r.state().pending_replacement.as_ref().unwrap();
                assert!(matches!(
                    r.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P0, .. }
                ));
                assert!(
                    matches!(pause.proposed, ProposedEvent::Damage { source_id, amount, .. }
                    if source_id == source && amount == if step == 0 { leg + 1 } else { leg + 2 })
                );
                assert_eq!(pause.candidates.len(), 3 - step);
                let mut old =
                    serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone()))
                        .unwrap();
                old["resolution_state_version"] = serde_json::json!(8);
                old["pending_replacement"]
                    .as_object_mut()
                    .unwrap()
                    .remove("damage_source_context");
                let restored: ResolutionStateWire = serde_json::from_value(old).unwrap();
                r = GameRunner::from_state(restored.into_game_state());
                let owner = r
                    .state()
                    .resolving_stack_entry
                    .as_ref()
                    .unwrap()
                    .ability()
                    .unwrap();
                assert!(owner.trigger_source.is_some());
                assert!(owner.sub_ability.is_some());
                let pause = r.state().pending_replacement.as_ref().unwrap();
                assert!(
                    pause.damage_source_context.is_none(),
                    "mixed legacy work stays ordinary"
                );
                let index = pause
                    .candidates
                    .iter()
                    .position(|id| *id == chosen)
                    .unwrap();
                assert!(engine::ai_support::legal_actions(r.state())
                    .contains(&GameAction::ChooseReplacement { index }));
                events.extend(
                    r.act(GameAction::ChooseReplacement { index })
                        .unwrap()
                        .events,
                );
            }
            if mixed && leg == 0 {
                u13_until(&mut r, |state| {
                    matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
                });
            }
        }
        finish(&mut r, &mut events);
        // CR 608.2c + CR 616.1f: each event applies +1, double, +2 once;
        // then the independent three-life instruction executes exactly once.
        let records: Vec<_> = r
            .state()
            .damage_dealt_this_turn
            .iter()
            .filter(|record| record.source_id == source)
            .collect();
        assert_eq!(
            records
                .iter()
                .map(|record| record.amount)
                .collect::<Vec<_>>(),
            if mixed { vec![6, 8] } else { vec![6] }
        );
        assert_eq!(r.life(P0), if mixed { 9 } else { 17 });
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == source)).count(), 1);
        assert!(r.state().pending_replacement.is_none());
        assert!(r.state().resolving_stack_entry.is_none());
        assert!(r.state().resolution_stack.is_empty());
    }
}

#[test]
fn u13_explicit_v7_ordinary_counter_removal_queue_has_no_ticket_or_child() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let target = s
        .add_creature(P0, "Synthetic old removal recipient", 2, 4)
        .id();
    s.with_counter(target, charge(), 3);
    for (name, modification) in [
        (
            "Synthetic old removal Double",
            QuantityModification::Times { factor: 2 },
        ),
        (
            "Synthetic old removal Minus",
            QuantityModification::Minus { value: 1 },
        ),
    ] {
        s.add_creature(P0, name, 2, 2).with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                .valid_card(TargetFilter::SpecificObject { id: target })
                .quantity_modification(modification),
        );
    }
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic old ordinary removal",
            true,
            "Remove two charge counters from target permanent.",
        )
        .id();
    let mut r = s.build();
    r.cast(spell).target_object(target).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        &r.state().pending_replacement.as_ref().unwrap().proposed,
        engine::types::proposed_event::ProposedEvent::RemoveCounter {
            result_binding: None,
            ..
        }
    ));
    assert!(r.state().return_result_frames.is_empty());
    let mut old =
        serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone())).unwrap();
    old["resolution_state_version"] = serde_json::json!(8);
    let restored: ResolutionStateWire = serde_json::from_value(old).unwrap();
    r = GameRunner::from_state(restored.into_game_state());
    let index = r
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .position(|id| r.state().objects[&id.source].name == "Synthetic old removal Minus")
        .unwrap();
    r.act(GameAction::ChooseReplacement { index }).unwrap();
    finish(&mut r, &mut Vec::new());
    assert_eq!(n(&r, target, &charge()), 1);
    assert!(r.state().return_result_frames.is_empty());
    assert!(r.state().stack.is_empty());
}

fn u13_reload_kind(r: GameRunner, persistence: u8) -> GameRunner {
    use engine::types::game_state::{PersistedGameState, TrustedGameStateEnvelope};
    let state = match persistence {
        0 => serde_json::from_value::<GameState>(serde_json::to_value(r.state()).unwrap()).unwrap(),
        1 => serde_json::from_value::<ResolutionStateWire>(
            serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone())).unwrap(),
        )
        .unwrap()
        .into_game_state(),
        2 => serde_json::from_value::<PersistedGameState>(
            serde_json::to_value(PersistedGameState::capture(r.state().clone())).unwrap(),
        )
        .unwrap()
        .into_game_state()
        .unwrap(),
        3 => serde_json::from_value::<TrustedGameStateEnvelope>(
            serde_json::to_value(TrustedGameStateEnvelope::capture(r.state().clone())).unwrap(),
        )
        .unwrap()
        .into_game_state(),
        _ => unreachable!(),
    };
    GameRunner::from_state(state)
}
