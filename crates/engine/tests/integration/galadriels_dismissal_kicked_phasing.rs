//! Issue #10266: kicked Galadriel's Dismissal must phase out each creature the
//! targeted player controls, not the targeted player themself.
//!
//! https://github.com/phase-rs/phase/issues/10266

use engine::game::game_object::PhaseOutCause;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Galadriel's Dismissal (LTC 500) — verbatim Oracle text.
const GALADRIELS_DISMISSAL: &str = "Kicker {2}{W} (You may pay an additional {2}{W} as you cast \
     this spell.)\nTarget creature phases out. If this spell was kicked, each creature target \
     player controls phases out instead. (Treat phased-out creatures and anything attached to \
     them as though they don't exist until their controller's next turn.)";

/// Bolt Bend — verbatim Oracle text.
const BOLT_BEND: &str = "This spell costs {3} less to cast if you control a creature with power 4 \
     or greater.\nChange the target of target spell or ability with a single target.";

/// Teferi, Timeless Voyager — verbatim Oracle text.
const TEFERI_TIMELESS_VOYAGER: &str =
    "[+1]: Draw a card.\n[−3]: Put target creature on top of its \
     owner's library.\n[−8]: Each creature target opponent controls phases out. Until the end of \
     your next turn, they can't phase in.";

fn white_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); n]
}

fn red_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); n]
}

/// CR 702.26b: only a permanent can phase out. The kicked branch's player
/// target is the controller anchor for "each creature target player controls",
/// so the player must remain phased in while every creature they control
/// phases out. Reverting the `collect_player_targets` player-scope gate makes
/// the player phased out and fails this test.
#[test]
fn kicked_galadriels_dismissal_phases_out_only_the_target_players_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Galadriel's Dismissal", true, GALADRIELS_DISMISSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let caster_creature = scenario.add_creature(P0, "Caster Bear", 2, 2).id();
    let target_creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    let other_target_creature = scenario.add_creature(P1, "Target Elf", 1, 1).id();
    // {W} base plus {2}{W} kicker.
    scenario.with_mana_pool(P0, white_mana(4));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_player(P1)
        .resolve();

    let state = outcome.state();
    assert!(
        !state.players[P0.0 as usize].is_phased_out(),
        "the caster must never phase out"
    );
    assert!(
        !state.players[P1.0 as usize].is_phased_out(),
        "the targeted player is the controller anchor for the creature filter, not a phasing target"
    );
    assert!(
        state.objects[&target_creature].is_phased_out(),
        "the targeted player's first creature must phase out"
    );
    assert!(
        state.objects[&other_target_creature].is_phased_out(),
        "each creature the targeted player controls must phase out"
    );
    assert!(
        state.objects[&caster_creature].is_phased_in(),
        "the caster's creature must remain phased in"
    );
    assert!(
        state.stack.is_empty(),
        "the kicked spell must finish resolving, stack={:?}",
        state.stack.len()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// CR 115.7a + CR 115.7d: with the kicked branch reduced to its announced
/// player target, Bolt Bend can target the spell and must be offered the
/// player pool — never the unkicked base branch's creature pool. Retargeting
/// to the caster phases out the caster's creatures instead.
#[test]
fn kicked_galadriels_dismissal_retargets_through_the_player_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dismissal = scenario
        .add_spell_to_hand_from_oracle(P0, "Galadriel's Dismissal", true, GALADRIELS_DISMISSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P1, "Bolt Bend", true, BOLT_BEND)
        .id();
    let caster_creature = scenario.add_creature(P0, "Caster Bear", 2, 2).id();
    let target_creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    scenario.with_mana_pool(P0, white_mana(4));
    scenario.with_mana_pool(P1, red_mana(4));

    let mut runner = scenario.build();
    runner
        .cast(dismissal)
        .accept_optional()
        .target_player(P1)
        .commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 may respond");

    let outcome = runner.cast(bolt_bend).target_object(dismissal).resolve();
    let WaitingFor::RetargetChoice {
        legal_new_targets,
        slot_pools,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "Bolt Bend must present the retarget choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(
        legal_new_targets.contains(&TargetRef::Player(P0)),
        "the caster must be a legal replacement player, got {legal_new_targets:?}"
    );
    assert!(
        !legal_new_targets
            .iter()
            .any(|target| matches!(target, TargetRef::Object(_))),
        "the base branch's creature pool must not leak into the player slot: {legal_new_targets:?}"
    );
    assert!(
        slot_pools
            .first()
            .is_some_and(|pool| pool.contains(&TargetRef::Player(P0))),
        "the per-slot pool must be the companion player pool, got {slot_pools:?}"
    );

    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Player(P0))],
        })
        .expect("retarget the kicked spell to the caster");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert!(
        state.objects[&caster_creature].is_phased_out(),
        "the caster's creature is now the affected population"
    );
    assert!(
        state.objects[&target_creature].is_phased_in(),
        "the original target's creature must stay phased in"
    );
    assert!(
        !state.players[P0.0 as usize].is_phased_out(),
        "the retargeted player is still only the population anchor"
    );
}

/// CR 702.26b + CR 101.2: Teferi's follow-on "they can't phase in" must bind
/// the creatures that actually phased out, not the player-only anchor. At the
/// opponent's next untap step they remain phased out while the restriction is
/// active.
#[test]
fn teferi_timeless_voyager_restriction_binds_the_phased_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Timeless Voyager",
            "Teferi",
            8,
            TEFERI_TIMELESS_VOYAGER,
        )
        .id();
    let first_opponent_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let second_opponent_creature = scenario.add_creature(P1, "Opponent Elf", 1, 1).id();
    // A noncreature permanent controlled by the opponent: Teferi's creature
    // filter never phases it out, so it is an unrestricted phase-in control.
    let control_permanent = scenario
        .add_artifact_from_oracle(P1, "Control Relic", "")
        .id();
    let own_creature = scenario.add_creature(P0, "Own Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.activate(teferi, 2).target_player(P1).resolve();

    assert!(
        runner.state().objects[&first_opponent_creature].is_phased_out(),
        "the opponent's first creature must phase out"
    );
    assert!(
        runner.state().objects[&second_opponent_creature].is_phased_out(),
        "the opponent's second creature must phase out"
    );
    assert!(
        !runner.state().players[P1.0 as usize].is_phased_out(),
        "the targeted opponent is the population anchor, not a phasing target"
    );
    assert!(
        runner.state().objects[&own_creature].is_phased_in(),
        "the controller's own creature must remain phased in"
    );

    // A control creature phased out WITHOUT Teferi's restriction must phase in
    // at the same untap. This proves the untap actually ran, rather than the
    // test stopping at an earlier combat prompt.
    let mut setup_events = Vec::new();
    phase_out_object(
        runner.state_mut(),
        control_permanent,
        PhaseOutCause::Directly,
        &mut setup_events,
    );
    assert!(runner.state().objects[&control_permanent].is_phased_out());

    // Drive to the opponent's first upkeep after their untap. Declaring no
    // attackers handles the combat prompt `advance_to_phase` can stop on.
    let starting_turn = runner.state().turn_number;
    for _ in 0..96 {
        if runner.state().active_player == P1
            && runner.state().turn_number > starting_turn
            && runner.state().phase == Phase::Upkeep
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the opponent's untap");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            other => {
                panic!("unexpected waiting_for while advancing to the opponent's untap: {other:?}")
            }
        }
    }
    assert_eq!(
        runner.state().active_player,
        P1,
        "the loop must reach the opponent's turn"
    );
    assert!(
        runner.state().turn_number > starting_turn,
        "the opponent's untap must advance the turn number"
    );
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the opponent's untap must have completed"
    );
    assert!(
        runner.state().objects[&control_permanent].is_phased_in(),
        "an unrestricted phased-out permanent must phase in at the opponent's untap"
    );
    assert!(
        runner.state().objects[&first_opponent_creature].is_phased_out(),
        "the restriction must be bound to the first phased creature"
    );
    assert!(
        runner.state().objects[&second_opponent_creature].is_phased_out(),
        "the restriction must be bound to every phased creature during the opponent's untap"
    );
    assert!(
        runner.state().objects[&own_creature].is_phased_in(),
        "Teferi's restriction must not affect the controller's creature"
    );
}

/// CR 608.2c: a completed phase action with an EMPTY affected population must
/// forward an empty result to a following `ParentTarget` rider. It must not
/// fall back to the player-only anchor and install a restriction on the player.
#[test]
fn teferi_empty_creature_population_binds_no_player_restriction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Timeless Voyager",
            "Teferi",
            8,
            TEFERI_TIMELESS_VOYAGER,
        )
        .id();

    let mut runner = scenario.build();
    runner.activate(teferi, 2).target_player(P1).resolve();

    assert!(
        !runner.state().players[P1.0 as usize].is_phased_out(),
        "an empty creature population must not phase the player anchor"
    );
    assert!(
        runner.state().transient_continuous_effects.is_empty(),
        "an empty phase result must bind no CantPhaseIn rider, got {:?}",
        runner.state().transient_continuous_effects
    );
}

/// The Doctor's Childhood Barn (WHO 579) — verbatim Oracle text.
const DOCTORS_CHILDHOOD_BARN: &str = "Creatures enter tapped.\nWhenever chaos ensues, for each \
     opponent, choose up to one target nonland permanent that opponent controls. Untap those \
     permanents. They phase out. They can't phase in for as long as The Doctor's Childhood Barn \
     remains face up. When a player planeswalks, those permanents phase in.";

/// CR 702.26g/h + CR 608.2c: the printed Barn chaos ability resolves through the
/// engine with a chosen host, a chosen Pacifism controlled by another opponent,
/// and an incidental Equipment attached to that host. Both chosen objects must
/// bind the can't-phase-in restriction and the delayed phase-in; the incidental
/// attachment must not be added to that forwarded population.
#[test]
fn childhood_barn_binds_selected_objects_not_incidental_attachments() {
    use engine::database::synthesis::synthesize_planechase;
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::deck_loading::create_planar_deck_card;
    use engine::game::effects::phase_out::resolve_phase_in;
    use engine::game::planechase::{chaos_ensues, restamp_planar_objects_to_controller};
    use engine::game::scenario::GameRunner;
    use engine::game::triggers::check_delayed_triggers;
    use engine::game::triggers::process_triggers;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::card::CardFace;
    use engine::types::card_type::CoreType;
    use engine::types::events::GameEvent;
    use engine::types::format::FormatConfig;
    use engine::types::player::PlayerId;
    use engine::types::triggers::TriggerMode;

    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_with_format(FormatConfig::planechase(), 3, 11);
    scenario.at_phase(Phase::PreCombatMain);

    // P1 controls the chosen host. P2 controls the chosen Aura attached to it,
    // so the selected Aura phases out indirectly with its selected host
    // (CR 702.26h). P1's incidental Equipment is attached to the same host but
    // is not selected.
    let chosen_host = scenario.add_creature(P1, "Chosen Host", 3, 3).id();
    let chosen_aura = scenario
        .add_enchantment_from_oracle(
            p2,
            "Pacifism",
            "Enchant creature\nEnchanted creature can't attack or block.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let incidental_equipment = scenario
        .add_artifact_from_oracle(
            P1,
            "Incidental Equipment",
            "Equipped creature gets +0/+1.\nEquip {0}",
        )
        .with_subtypes(vec!["Equipment"])
        .id();

    let parsed = parse_oracle_text(
        DOCTORS_CHILDHOOD_BARN,
        "The Doctor's Childhood Barn",
        &[],
        &["Plane".to_string()],
        &[],
    );
    let mut face = CardFace {
        name: "The Doctor's Childhood Barn".to_string(),
        card_type: engine::types::card_type::CardType {
            core_types: vec![CoreType::Plane],
            ..Default::default()
        },
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..CardFace::default()
    };
    synthesize_planechase(&mut face);

    let mut runner = scenario.build();
    let barn = {
        let state = runner.state_mut();
        state.objects.get_mut(&chosen_aura).unwrap().attached_to = Some(chosen_host.into());
        state
            .objects
            .get_mut(&chosen_host)
            .unwrap()
            .attachments
            .push(chosen_aura);
        state
            .objects
            .get_mut(&incidental_equipment)
            .unwrap()
            .attached_to = Some(chosen_host.into());
        state
            .objects
            .get_mut(&chosen_host)
            .unwrap()
            .attachments
            .push(incidental_equipment);
        engine::game::layers::mark_layers_full(state);

        let barn = create_planar_deck_card(state, &face, P0);
        state.planar_deck.retain(|id| *id != barn);
        state.objects.get_mut(&barn).unwrap().face_down = false;
        state.command_zone.push_back(barn);
        state.planar_controller = Some(P0);
        restamp_planar_objects_to_controller(state);
        barn
    };

    // The printed trigger/target-slot wiring must still offer the chosen
    // permanents. This runs on a clone so the direct-resolution fixture below
    // can supply both opponents' choices explicitly.
    {
        let mut prompt_runner = GameRunner::from_state(runner.state().clone());
        let mut prompt_events = Vec::new();
        chaos_ensues(prompt_runner.state_mut(), &mut prompt_events);
        process_triggers(prompt_runner.state_mut(), &prompt_events);
        let mut offered = None;
        for _ in 0..20 {
            match prompt_runner.state().waiting_for.clone() {
                WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                    offered = Some(
                        target_slots
                            .iter()
                            .flat_map(|slot| slot.legal_targets.iter())
                            .filter_map(|target| match target {
                                TargetRef::Object(id) => Some(*id),
                                TargetRef::Player(_) => None,
                            })
                            .collect::<Vec<_>>(),
                    );
                    break;
                }
                WaitingFor::Priority { .. } => {
                    prompt_runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority toward Barn's target prompt");
                }
                other => panic!("unexpected prompt before Barn target selection: {other:?}"),
            }
        }
        let offered = offered.expect("Barn's chaos trigger must raise a target prompt");
        assert!(
            offered.contains(&chosen_host) && offered.contains(&chosen_aura),
            "Barn's target prompt must offer the chosen host and Pacifism, got {offered:?}"
        );
    }

    let execute = face
        .triggers
        .iter()
        .find(|trigger| matches!(trigger.mode, TriggerMode::ChaosEnsues))
        .and_then(|trigger| trigger.execute.as_ref())
        .expect("printed Barn chaos trigger");
    let mut ability = build_resolved_from_def(execute, barn, P0);
    ability.targets = vec![
        TargetRef::Object(chosen_host),
        TargetRef::Object(chosen_aura),
    ];
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Barn's printed chaos ability resolves");

    {
        let state = runner.state();
        for (object_id, label) in [
            (chosen_host, "chosen host"),
            (chosen_aura, "chosen Aura"),
            (incidental_equipment, "incidental Equipment"),
        ] {
            assert!(
                state.objects[&object_id].is_phased_out(),
                "{label} must phase out"
            );
        }
        let delayed_with_selection = state.delayed_triggers.iter().any(|trigger| {
            let targets: Vec<ObjectId> = trigger
                .ability
                .targets
                .iter()
                .filter_map(|target| match target {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            targets.contains(&chosen_host) && targets.contains(&chosen_aura)
        });
        assert!(
            delayed_with_selection,
            "a delayed phase-in must bind both selected objects"
        );
        for delayed in &state.delayed_triggers {
            let targets: Vec<ObjectId> = delayed
                .ability
                .targets
                .iter()
                .filter_map(|target| match target {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            assert!(
                !targets.contains(&incidental_equipment),
                "a delayed phase-in must not add the incidental attachment, got {targets:?}"
            );
        }
    }

    // The restriction is observable through the resolver: while Barn is face
    // up, an explicit phase-in of either selected object is a no-op.
    for object_id in [chosen_host, chosen_aura] {
        let phase_in = engine::types::ability::ResolvedAbility::new(
            engine::types::ability::Effect::PhaseIn {
                target: engine::types::ability::TargetFilter::SpecificObject { id: object_id },
            },
            vec![TargetRef::Object(object_id)],
            barn,
            P0,
        );
        let mut phase_in_events = Vec::new();
        resolve_phase_in(runner.state_mut(), &phase_in, &mut phase_in_events)
            .expect("explicit phase-in resolves");
        assert!(
            runner.state().objects[&object_id].is_phased_out(),
            "a selected object must remain locked while Barn is face up"
        );
    }
    // The incidental attachment has no direct restriction: an explicit
    // phase-in succeeds. Put it back out so the later delayed host phase-in
    // proves it rides the host rather than a selected delayed entry.
    {
        let state = runner.state_mut();
        let phase_in = engine::types::ability::ResolvedAbility::new(
            engine::types::ability::Effect::PhaseIn {
                target: engine::types::ability::TargetFilter::SpecificObject {
                    id: incidental_equipment,
                },
            },
            vec![TargetRef::Object(incidental_equipment)],
            barn,
            P0,
        );
        let mut phase_in_events = Vec::new();
        resolve_phase_in(state, &phase_in, &mut phase_in_events)
            .expect("incidental explicit phase-in resolves");
        assert!(
            state.objects[&incidental_equipment].is_phased_in(),
            "the incidental attachment must not be separately restricted"
        );
        let mut phase_out_events = Vec::new();
        // Restore the incidental attachment's indirect phase-out so the host's
        // delayed phase-in cascades it in (CR 702.26g).
        phase_out_object(
            state,
            incidental_equipment,
            PhaseOutCause::Indirectly,
            &mut phase_out_events,
        );
    }

    // CR 702.26h + CR 611.2b: while Barn is face up the lock holds; once it
    // planeswalks away the delayed instruction phases the selected objects in.
    {
        let state = runner.state_mut();
        state.command_zone.retain(|id| *id != barn);
        state.objects.get_mut(&barn).unwrap().face_down = true;
        engine::game::layers::mark_layers_full(state);
    }
    let planeswalk = GameEvent::Planeswalked {
        player_id: P0,
        from: Some(barn),
        to: None,
    };
    check_delayed_triggers(runner.state_mut(), &[planeswalk]);
    let mut resolve_events = Vec::new();
    while !runner.state().stack.is_empty() {
        engine::game::stack::resolve_top(runner.state_mut(), &mut resolve_events);
    }
    assert!(
        runner.state().objects[&chosen_host].is_phased_in(),
        "the delayed instruction must phase in the chosen host"
    );
    assert!(
        runner.state().objects[&chosen_aura].is_phased_in(),
        "the delayed instruction must phase in the chosen Aura"
    );
    assert!(
        runner.state().objects[&incidental_equipment].is_phased_in(),
        "the incidental attachment phases in with its host, not as a selected delayed target"
    );
}
