//! Regression: Swans of Bryn Argoll's prevention follow-up draws cards for
//! the *damage source's* controller, not Swans's controller.
//!
//! Oracle text: "If a source would deal damage to ~, prevent that damage.
//! The source's controller draws cards equal to the damage prevented this way."
//!
//! Historical bug: `Effect::Draw` short-circuited on `target.is_context_ref()`
//! and returned `ability.controller` (Swans's controller — the prevented
//! player), ignoring the auto-resolved target slot. The fix routes context-ref
//! targets through `resolve_player_for_context_ref`, which reads
//! `state.post_replacement_event_source` and returns the damage source's
//! controller.
//!
//! CR 615.5: Some prevention effects also include an additional effect, which
//!           may refer to the amount of damage that was prevented.
//! CR 609.7: Some effects apply to damage from a source — "the source's
//!           controller draws cards equal to the damage prevented this way".
//! CR 121.1: A player draws a card by putting the top card of their library
//!           into their hand.

use std::collections::HashSet;

use engine::game::effects;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::TargetRef;
use engine::types::ability::{Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{
    DrainStatus, GameState, PersistedGameState, PostReplacementDrain, ResidentDrainPolicy,
};
use engine::types::identifiers::{CardId, ObjectId, ObjectIdentityBinding, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn assert_restored_state(restored: &GameState, prepared: &GameState) {
    let mut expected = prepared.clone();
    // This runtime invalidation counter is deliberately omitted from saved games.
    // Every persisted authority and all other state still compare exactly.
    expected.state_revision = restored.state_revision;
    assert_eq!(restored, &expected);
}

fn install_turn_replacement(
    runner: &mut engine::game::scenario::GameRunner,
    host: ObjectId,
    replacement: engine::types::ability::ReplacementDefinition,
) {
    let mut installation = ResolvedAbility::new(
        Effect::AddTargetReplacement {
            replacement: Box::new(replacement),
            target: TargetFilter::SelfRef,
        },
        vec![],
        host,
        runner.state().objects[&host].controller,
    );
    // CR 611.2a + CR 615.3: these surrounding runtime shields last this turn,
    // retaining consumed state through the real resolution-install authority.
    installation.duration = Some(engine::types::ability::Duration::UntilEndOfTurn);
    effects::resolve_ability_chain(runner.state_mut(), &installation, &mut Vec::new(), 0)
        .expect("install the surrounding turn-bound replacement");
    assert!(runner.state().objects[&host]
        .replacement_definitions
        .last()
        .unwrap()
        .is_resolution_installed());
}

#[test]
fn swans_followup_draws_for_damage_sources_controller() {
    let mut state = GameState::new_two_player(42);
    let swans_controller = PlayerId(0);
    let damage_source_controller = PlayerId(1);

    // Stack the libraries so we can detect a draw on player 1.
    // Use plain Forest stand-ins — content doesn't matter.
    let p1_lib_card = create_object(
        &mut state,
        CardId(1),
        damage_source_controller,
        "Forest".to_string(),
        Zone::Library,
    );
    state.players[1].library.push_back(p1_lib_card);

    // The "damage source" object the prevention shield captured.
    let damage_source = create_object(
        &mut state,
        CardId(2),
        damage_source_controller,
        "Goblin".to_string(),
        Zone::Battlefield,
    );

    // Simulate the state the prevention applier's `Prevented` arm leaves behind at
    // the moment the follow-up runs:
    // - `last_effect_count` carries the prevented amount (1 damage)
    // - the drain carries the damage source's id as its prevented-event source
    //
    // The drain is `Dispatching`, not `Ready`: production reaches this point with
    // the continuation already taken (it is the thing currently running) but its
    // event context still readable, which is exactly what
    // `PostReplacementSourceController` reads below.
    state.last_effect_count = Some(1);
    state.install_post_replacement_drain(
        PostReplacementDrain {
            status: DrainStatus::Dispatching,
            source: None,
            applied: HashSet::new(),
            event_source: Some(damage_source),
            event_source_identity: Some(ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&state.objects[&damage_source]),
                Zone::Battlefield,
            )),
            event_target: None,
            // CR 109.5: this fixture drives the event-context read directly and
            // installs no replacing object, so "you" falls back to the affected
            // object's controller exactly as before the slot existed.
            controller: None,
        },
        ResidentDrainPolicy::Replace,
    );

    let p1_hand_before = state.players[1].hand.len();
    let p0_hand_before = state.players[0].hand.len();

    // Build the parsed shape Swans's follow-up lowers to:
    //   Draw { count: EventContextAmount, target: PostReplacementSourceController }
    // ResolvedAbility's source/controller mirror what
    // `apply_post_replacement_effect` would supply: Swans (P0).
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            target: TargetFilter::PostReplacementSourceController,
        },
        vec![],
        ObjectId(999), // Swans
        swans_controller,
    );

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[1].hand.len(),
        p1_hand_before + 1,
        "damage source's controller (P1) must draw 1 card"
    );
    assert_eq!(
        state.players[0].hand.len(),
        p0_hand_before,
        "Swans's controller (P0) must NOT draw — the follow-up routes to the \
         prevented event's source controller, not the shield's controller"
    );
}

const SWANS_ORACLE: &str = "Flying\nIf a source would deal damage to this creature, prevent that damage. The source's controller draws cards equal to the damage prevented this way.";
const MOGG_ORACLE: &str = "Sacrifice this creature: It deals 1 damage to any target.";
const BLOODFIRE_ORACLE: &str =
    "{R}, Sacrifice this creature: It deals 6 damage to each creature and each player.";
const GHOST_LIT_RAIDER_ORACLE: &str = "{2}{R}, {T}: This creature deals 2 damage to target creature.\nChannel — {3}{R}, Discard this card: It deals 4 damage to target creature.";
const COMEUPPANCE_ORACLE: &str = "Prevent all damage that would be dealt to you and planeswalkers you control this turn by sources you don't control. If damage from a creature source is prevented this way, Comeuppance deals that much damage to that creature. If damage from a noncreature source is prevented this way, Comeuppance deals that much damage to the source's controller.";
const ZURS_WEIRDING_ORACLE: &str = "Players play with their hands revealed.\nIf a player would draw a card, they reveal it instead. Then any other player may pay 2 life. If a player does, put that card into its owner's graveyard. Otherwise, that player draws a card.";
const NEW_WAY_FORWARD_ORACLE: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. When damage is prevented this way, New Way Forward deals that much damage to that source's controller and you draw that many cards.";

/// CR 707.10 + CR 201.5 + CR 615.5: each actual spell copy is its own
/// damage source. Keeping or changing a copied target never changes that source.
#[test]
fn real_twincast_birth_binds_the_new_spell_source_before_saved_retarget_and_swans() {
    use engine::game::scenario::GameRunner;
    use engine::types::game_state::PersistedGameState;
    use engine::types::mana::ManaCost;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for change_target in [false, true] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let swans = scenario
            .add_creature_from_oracle(p3, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
            .controlled_by(p2)
            .id();
        let other_swans = scenario
            .add_creature_from_oracle(p2, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
            .controlled_by(p3)
            .id();
        let bolt = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let twincast = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Twincast",
                true,
                "Copy target instant or sorcery spell. You may choose new targets for the copy.",
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        for player in [P0, P1, p2, p3] {
            scenario.with_library_top(player, &["First", "Second", "Third", "Fourth"]);
        }
        let mut runner = scenario.build();
        assert!(runner.state().objects[&bolt].parse_warnings.is_empty());
        assert!(matches!(
            *runner.state().objects[&bolt].abilities[0].effect,
            Effect::DealDamage { .. }
        ));
        assert!(runner.state().objects[&twincast].parse_warnings.is_empty());
        assert!(matches!(
            *runner.state().objects[&twincast].abilities[0].effect,
            Effect::CopySpell { .. }
        ));
        let mut events = runner.act(GameAction::PassPriority).unwrap().events;
        runner.cast(bolt).target_object(swans).commit();
        let original_pin = runner
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .context
            .source_identity
            .unwrap();
        assert_eq!(original_pin.reference.object_id, bolt);
        assert_eq!(original_pin.expected_zone, Zone::Stack);
        for _ in 0..3 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        events.extend(
            runner
                .cast(twincast)
                .target_object(bolt)
                .resolve()
                .events()
                .iter()
                .cloned(),
        );
        let WaitingFor::CopyRetarget {
            copy_id, player, ..
        } = runner.state().waiting_for
        else {
            panic!(
                "Twincast must produce its real retarget prompt: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P0);
        assert_ne!(copy_id, bolt);
        assert_ne!(copy_id, twincast);
        let copy_pin = ObjectIdentityBinding::new(
            ObjectIncarnationRef::from_object(&runner.state().objects[&copy_id]),
            Zone::Stack,
        );
        let copy = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap()
            .ability()
            .unwrap();
        assert_eq!(copy.source_id, copy_id);
        assert_eq!(copy.context.source_identity, Some(copy_pin));
        assert_eq!(
            copy.source_incarnation,
            Some(copy_pin.reference.incarnation)
        );
        assert_eq!(copy.targets, vec![TargetRef::Object(swans)]);
        assert_ne!(copy_pin, original_pin);
        let canonical = runner.state().clone();
        for view in [
            engine::game::visibility::filter_state_for_viewer(&canonical, P0),
            engine::game::visibility::filter_state_for_viewer(&canonical, P1),
            engine::game::visibility::filter_state_for_viewer(&canonical, p2),
            engine::game::visibility::filter_state_for_unseated_viewer(&canonical),
        ] {
            assert_eq!(
                view.stack
                    .iter()
                    .find(|entry| entry.id == copy_id)
                    .unwrap()
                    .ability()
                    .unwrap()
                    .context
                    .source_identity,
                Some(copy_pin)
            );
        }
        assert_eq!(runner.state(), &canonical);
        let saved = serde_json::to_string(&PersistedGameState::capture(canonical.clone())).unwrap();
        let restored = serde_json::from_str::<PersistedGameState>(&saved)
            .unwrap()
            .into_game_state()
            .unwrap();
        let prepared = PersistedGameState::capture(canonical)
            .into_game_state()
            .unwrap();
        assert_restored_state(&restored, &prepared);
        runner = GameRunner::from_state(restored);
        events.extend(
            runner
                .act(if change_target {
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(other_swans)),
                    }
                } else {
                    GameAction::KeepAllCopyTargets
                })
                .unwrap()
                .events,
        );
        for _ in 0..40 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "{:?}",
                runner.state().waiting_for
            );
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::SpellCopied { object_id, .. } if *object_id == copy_id)).count(), 1);
        for (source, target) in [
            (bolt, swans),
            (copy_id, if change_target { other_swans } else { swans }),
        ] {
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamagePrevented { source_id, target: TargetRef::Object(actual), amount: 3 } if *source_id == source && *actual == target)).count(), 1);
        }
        for player in [P0, P1] {
            assert_eq!(runner.state().players[player.0 as usize].hand.len(), 3);
        }
        for player in [p2, p3] {
            assert!(runner.state().players[player.0 as usize].hand.is_empty());
        }
        assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&twincast].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 707.10b + CR 113.7a + CR 608.2h: copying an activated ability
/// preserves its departed source, including its original battlefield occurrence.
#[test]
fn real_lithoform_ability_copy_keeps_the_sacrificed_moggs_exact_source_at_saved_retarget() {
    use engine::game::scenario::GameRunner;
    use engine::types::game_state::PersistedGameState;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let swans = scenario
        .add_creature_from_oracle(p3, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
        .controlled_by(p2)
        .id();
    let mogg = scenario
        .add_creature_from_oracle(P0, "Mogg Fanatic", 1, 1, MOGG_ORACLE)
        .controlled_by(P1)
        .id();
    let lithoform = scenario.add_creature(P1, "Lithoform Engine", 0, 0).as_artifact().from_oracle_text(
        "{2}, {T}: Copy target activated or triggered ability you control. You may choose new targets for the copy.\n{3}, {T}: Copy target instant or sorcery spell you control. You may choose new targets for the copy.\n{4}, {T}: Copy target permanent spell you control. (The copy becomes a token.)").id();
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    for player in [P0, P1, p2, p3] {
        scenario.with_library_top(player, &["First", "Second", "Third"]);
    }
    let mut runner = scenario.build();
    let original_pin = ObjectIdentityBinding::new(
        ObjectIncarnationRef::from_object(&runner.state().objects[&mogg]),
        Zone::Battlefield,
    );
    assert!(runner.state().objects[&lithoform].parse_warnings.is_empty());
    assert!(matches!(
        *runner.state().objects[&lithoform].abilities[0].effect,
        Effect::CopySpell { .. }
    ));
    let mut events = runner.act(GameAction::PassPriority).unwrap().events;
    events.extend(
        runner
            .act(GameAction::ActivateAbility {
                source_id: mogg,
                ability_index: 0,
            })
            .unwrap()
            .events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(swans)),
            })
            .unwrap()
            .events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert_eq!(runner.state().objects[&mogg].zone, Zone::Graveyard);
    assert_ne!(
        runner.state().objects[&mogg].incarnation,
        original_pin.reference.incarnation
    );
    let original_id = runner.state().stack.back().unwrap().id;
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .context
            .source_identity,
        Some(original_pin)
    );
    events.extend(
        runner
            .act(GameAction::ActivateAbility {
                source_id: lithoform,
                ability_index: 0,
            })
            .unwrap()
            .events,
    );
    // The original Mogg ability is the sole legal controlled ability.
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .targets,
        vec![TargetRef::Object(original_id)]
    );
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    let WaitingFor::CopyRetarget {
        copy_id, player, ..
    } = runner.state().waiting_for
    else {
        panic!("actual ability copy must offer retarget");
    };
    assert_eq!(player, P1);
    assert_ne!(copy_id, original_id);
    assert_ne!(copy_id, mogg);
    for id in [original_id, copy_id] {
        let entry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == id)
            .unwrap();
        assert_eq!(entry.source_id, mogg);
        let ability = entry.ability().unwrap();
        assert_eq!(ability.source_id, mogg);
        assert_eq!(ability.context.source_identity, Some(original_pin));
        assert_eq!(
            ability.source_incarnation,
            Some(original_pin.reference.incarnation)
        );
        assert_eq!(ability.targets, vec![TargetRef::Object(swans)]);
    }
    let saved =
        serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
    let restored = serde_json::from_str::<PersistedGameState>(&saved)
        .unwrap()
        .into_game_state()
        .unwrap();
    let prepared = PersistedGameState::capture(runner.state().clone())
        .into_game_state()
        .unwrap();
    assert_restored_state(&restored, &prepared);
    runner = GameRunner::from_state(restored);
    events.extend(runner.act(GameAction::KeepAllCopyTargets).unwrap().events);
    for _ in 0..40 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().players[1].hand.len(), 2);
    for player in [P0, p2, p3] {
        assert!(runner.state().players[player.0 as usize].hand.is_empty());
    }
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, player_id: P1 } if *object_id == mogg)).count(), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 1 } if *source_id == mogg && *target == swans)).count(), 2);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 400.7j + CR 608.2h + CR 615.5: only the immediate forwarded instruction
/// sources the returned occurrence. Its independent tail still sources the spell.
#[test]
fn actual_forwarded_zone_move_rebinds_only_the_immediate_saved_damage_source() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, SubAbilityLink, TypedFilter,
    };
    use engine::types::game_state::PersistedGameState;
    use engine::types::mana::ManaCost;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for (parked_move, performs_move) in [(false, true), (true, true), (true, false), (false, false)]
    {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let returned = scenario
            .add_creature_to_graveyard(P0, "Forwarded borrowed dealer", 2, 5)
            .id();
        let swans = scenario
            .add_creature_from_oracle(p3, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
            .controlled_by(p2)
            .id();
        for player in [P0, P1, p2, p3] {
            scenario.with_library_top(player, &["First", "Second"]);
        }
        let mut damage = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
        );
        damage.instruction_actor = Some(ControllerRef::SpecificPlayer { id: p3 }.into());
        damage.optional = true;
        let mut tail = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::OriginalController,
            },
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        tail.instruction_actor = Some(ControllerRef::You.into());
        damage.sub_ability = Some(Box::new(tail));
        let mut root = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(if !parked_move && !performs_move {
                    Zone::Exile
                } else {
                    Zone::Graveyard
                }),
                destination: Zone::Battlefield,
                target: TargetFilter::SpecificObject { id: returned },
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::SpecificPlayer { id: P1 }),
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        );
        root.forward_result = true;
        root.optional = parked_move;
        root.sub_ability = Some(Box::new(damage));
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic forwarded source", false)
            .with_ability_definition(root)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let old = ObjectIncarnationRef::from_object(&runner.state().objects[&returned]);
        let mut committed = runner.cast(spell).target_object(swans).commit();
        let mut events = Vec::new();
        for _ in 0..20 {
            if committed.state().stack.is_empty()
                || !matches!(committed.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            events.extend(committed.act(GameAction::PassPriority).unwrap().events);
        }
        drop(committed);
        if parked_move {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player: P0, .. }
            ));
            assert_eq!(runner.state().objects[&returned].zone, Zone::Graveyard);
            events.extend(
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: performs_move,
                    })
                    .unwrap()
                    .events,
            );
        }
        if !performs_move {
            for _ in 0..20 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                    "empty/declined producer cannot offer its dependent damage: {:?}",
                    runner.state().waiting_for
                );
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            assert_eq!(runner.state().objects[&returned].zone, Zone::Graveyard);
            assert_eq!(
                ObjectIncarnationRef::from_object(&runner.state().objects[&returned]),
                old
            );
            assert!(events.iter().all(|event| !matches!(
                event,
                GameEvent::DamagePrevented { .. } | GameEvent::DamageDealt { .. }
            )));
            for player in [P0, P1, p2, p3] {
                assert!(runner.state().players[player.0 as usize].hand.is_empty());
            }
            assert_eq!(runner.life(P0), 22);
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::GainLife, source_id, .. } if *source_id == spell)).count(), 1);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert!(runner.state().stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            continue;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == p3)
        );
        assert_eq!(runner.state().objects[&returned].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&returned].controller, P1);
        let pin = ObjectIdentityBinding::new(
            ObjectIncarnationRef::from_object(&runner.state().objects[&returned]),
            Zone::Battlefield,
        );
        assert_ne!(pin.reference, old);
        let frame = runner.state().active_optional_effect_frame().unwrap();
        assert_eq!(frame.ability.source_id, returned);
        assert_eq!(frame.ability.context.source_identity, Some(pin));
        assert_eq!(
            frame.ability.source_incarnation,
            Some(pin.reference.incarnation)
        );
        assert_eq!(frame.ability.controller, p3);
        assert_eq!(frame.ability.original_controller, Some(P0));
        assert_eq!(frame.ability.targets, vec![TargetRef::Object(swans)]);
        assert_eq!(frame.ability.sub_ability.as_ref().unwrap().source_id, spell);
        assert_eq!(
            frame
                .ability
                .sub_ability
                .as_ref()
                .unwrap()
                .instruction_actor,
            Some(ControllerRef::You.into())
        );
        let saved =
            serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_str::<PersistedGameState>(&saved)
            .unwrap()
            .into_game_state()
            .unwrap();
        let prepared = PersistedGameState::capture(runner.state().clone())
            .into_game_state()
            .unwrap();
        assert_restored_state(&restored, &prepared);
        runner = GameRunner::from_state(restored);
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap()
                .events,
        );
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "{:?}",
                runner.state().waiting_for
            );
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(runner.state().players[1].hand.len(), 1);
        for player in [P0, p2, p3] {
            assert!(runner.state().players[player.0 as usize].hand.is_empty());
        }
        assert_eq!(runner.life(P0), 22);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 1 } if *source_id == returned && *target == swans)).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::GainLife, source_id, .. } if *source_id == spell)).count(), 1);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 608.2c + CR 615.5: Instead selects the instruction's general event
/// separately from its valid actor and the parent's own-source occurrence.
#[test]
fn actual_selected_instead_keeps_its_own_event_with_an_independent_actor_after_saved_choice() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityCondition, ControllerRef, DamageTargetFilter, DamageTargetPlayerScope,
        PreventionAmount, ReplacementDefinition, ReplacementEventContext, SubAbilityLink,
    };
    use engine::types::game_state::PersistedGameState;
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for missing in [false, true] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let donor = scenario
            .add_creature(P1, "Selected event source", 2, 5)
            .controlled_by(p2)
            .id();
        let host = scenario
            .add_enchantment_from_oracle(P0, "Synthetic Instead prevention", "")
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .id();
        let mut runner = scenario.build();
        let own = ObjectIdentityBinding::new(
            ObjectIncarnationRef::from_object(&runner.state().objects[&host]),
            Zone::Battlefield,
        );
        let selected = ReplacementEventContext {
            source_id: Some(donor),
            source_identity: (!missing).then(|| {
                ObjectIdentityBinding::new(
                    ObjectIncarnationRef::from_object(&runner.state().objects[&donor]),
                    Zone::Battlefield,
                )
            }),
            event_target: Some(TargetRef::Player(p3)),
        };
        let mut sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::PostReplacementSourceController,
                damage_source: None,
                excess: None,
            },
            vec![],
            host,
            P0,
        );
        sub.instruction_actor = Some(ControllerRef::SpecificPlayer { id: p3 }.into());
        sub.context.replacement_event = Some(selected.clone());
        sub.condition = Some(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::IsYourTurn),
        });
        sub.optional = true;
        let mut tail = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::OriginalController,
            },
            vec![],
            host,
            P0,
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        sub.sub_ability = Some(Box::new(tail));
        let mut parent = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            host,
            P0,
        );
        parent.source_incarnation = Some(own.reference.incarnation);
        parent.context.source_identity = Some(own);
        parent.sub_ability = Some(Box::new(sub));
        let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All)
            .damage_target_filter(DamageTargetFilter::Player {
                player: DamageTargetPlayerScope::Specific(P1),
            })
            .runtime_execute(parent);
        shield.consume_on_apply = true;
        install_turn_replacement(&mut runner, host, shield);
        let mut committed = runner.cast(spell).target_player(P1).commit();
        let mut events = Vec::new();
        for _ in 0..20 {
            if committed.state().stack.is_empty()
                || !matches!(committed.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            events.extend(committed.act(GameAction::PassPriority).unwrap().events);
        }
        drop(committed);
        if !missing {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == p3)
            );
            let frame = runner.state().active_optional_effect_frame().unwrap();
            assert_eq!(
                frame.ability.context.replacement_event.as_ref(),
                Some(&selected)
            );
            assert_eq!(frame.ability.context.source_identity, Some(own));
            assert_eq!(frame.ability.source_id, host);
            assert_eq!(frame.ability.controller, p3);
            assert_eq!(frame.ability.original_controller, Some(P0));
            let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
                .unwrap();
            let restored = serde_json::from_str::<PersistedGameState>(&saved)
                .unwrap()
                .into_game_state()
                .unwrap();
            let prepared = PersistedGameState::capture(runner.state().clone())
                .into_game_state()
                .unwrap();
            assert_restored_state(&restored, &prepared);
            runner = GameRunner::from_state(restored);
            events.extend(
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap()
                    .events,
            );
        }
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "{:?}",
                runner.state().waiting_for
            );
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(
            runner.life(P0),
            23,
            "independent tail once, base instruction replaced"
        );
        assert_eq!(runner.life(P1), 20, "original damage was prevented");
        assert_eq!(runner.life(p2), if missing { 20 } else { 18 });
        assert_eq!(
            runner.life(p3),
            20,
            "actor is not a source-relative recipient"
        );
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamagePrevented { source_id, target: TargetRef::Player(P1), amount: 3 } if *source_id == spell)).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(target), amount: 2, .. } if *source_id == host && *target == p2)).count(), usize::from(!missing));
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 608.2c + CR 109.5 + CR 615.5: contextual replacement subjects and
/// independent printed "you" remain different actors. These explicitly
/// synthetic admitted sentences expose the actor through CounterAdded.
#[test]
fn replacement_subject_actors_are_separate_from_replacer_owner_and_independent_you() {
    use engine::types::counter::CounterType;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for family in 0..4 {
        let (oracle, replacer, actor) = match family {
            0 => ("If a player would gain life, instead that player puts a +1/+1 counter on this creature. You draw a card.", P0, p2),
            1 => ("If damage would be dealt to a player, prevent that damage and that player puts a +1/+1 counter on this creature. You draw a card.", P0, p2),
            2 => ("If a source would deal damage to this creature, prevent that damage. The source's controller puts a +1/+1 counter on this creature. You draw a card.", p2, P1),
            3 => ("If an opponent would draw two or more cards, instead that player puts a +1/+1 counter on this creature. You draw a card.", P0, p2),
            _ => unreachable!(),
        };
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario
            .add_creature_from_oracle(p3, "Synthetic actor-sensitive replacement", 2, 10, oracle)
            .controlled_by(replacer)
            .id();
        let source = if family == 2 {
            scenario
                .add_creature_from_oracle(P0, "Mogg Fanatic", 1, 1, MOGG_ORACLE)
                .controlled_by(P1)
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic event source",
                    true,
                    if family == 0 {
                        "Target player gains 2 life."
                    } else if family == 3 {
                        "Target player draws two cards."
                    } else {
                        "Synthetic event source deals 1 damage to target player."
                    },
                )
                .id()
        };
        for player in [P0, P1, p2, p3] {
            scenario.with_library_top(player, &["One", "Two", "Three"]);
        }
        let mut runner = scenario.build();
        assert!(
            runner.state().objects[&host].parse_warnings.is_empty(),
            "{:?}",
            runner.state().objects[&host].parse_warnings
        );
        assert_eq!(
            runner.state().objects[&host].replacement_definitions.len(),
            1
        );
        runner.act(GameAction::PassPriority).unwrap();
        let outcome = if family == 2 {
            runner.activate(source, 0).target_object(host).resolve()
        } else {
            runner.cast(source).target_player(p2).resolve()
        };
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CounterAdded { actor: actual, object_id, counter_type: CounterType::Plus1Plus1, count: 1 } if *actual == actor && *object_id == host)).count(), 1);
        assert_eq!(
            outcome.state().objects[&host]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&1)
        );
        assert_eq!(outcome.state().objects[&host].owner, p3);
        for player in [P0, P1, p2, p3] {
            outcome.assert_hand_drawn(player, i64::from(player == replacer));
        }
        assert_eq!(
            outcome.state().players[p2.0 as usize].life,
            20,
            "the original life/damage event was replaced"
        );
        assert!(outcome.state().stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 121.1 + CR 614.6: Alms's two subjects name its controller and the
/// original drawer, independently of the caster and the Collector's owner.
#[test]
fn real_alms_draw_replacement_keeps_you_and_the_original_drawer_separate() {
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for count in [1, 2] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let collector = scenario.add_creature_from_oracle(p3, "Alms Collector", 3, 4,
            "Flash\nIf an opponent would draw two or more cards, instead you and that player each draw a card.").controlled_by(P0).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Synthetic incoming draw",
                true,
                if count == 1 {
                    "Target player draws a card."
                } else {
                    "Target player draws two cards."
                },
            )
            .id();
        for player in [P0, P1, p2, p3] {
            scenario.with_library_top(player, &["First", "Second", "Third"]);
        }
        let mut runner = scenario.build();
        assert!(runner.state().objects[&collector].parse_warnings.is_empty());
        assert_eq!(
            runner.state().objects[&collector]
                .replacement_definitions
                .len(),
            1
        );
        let top0 = *runner.state().players[0].library.front().unwrap();
        let top2 = *runner.state().players[p2.0 as usize]
            .library
            .front()
            .unwrap();
        runner.act(GameAction::PassPriority).unwrap();
        let outcome = runner.cast(spell).target_player(p2).resolve();
        outcome.assert_hand_drawn(P0, i64::from(count == 2));
        outcome.assert_hand_drawn(p2, 1);
        for player in [P1, p3] {
            outcome.assert_hand_drawn(player, 0);
        }
        assert!(outcome.state().players[p2.0 as usize].hand.contains(&top2));
        assert_eq!(
            outcome.state().objects[&top0].zone,
            if count == 2 {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CardDrawn { player_id, object_id, .. } if *player_id == p2 && *object_id == top2)).count(), 1);
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, object_id, .. } if *object_id == top0)).count(), usize::from(count == 2));
        assert!(outcome.state().stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 701.20a + CR 614.6 + CR 608.2c: Zur's actual reveal head supplies
/// the replaced drawer actor before its patient is normalized. The following
/// counters are explicitly synthetic observations of that retained actor.
#[test]
fn real_zur_reveal_subject_actor_survives_normalization_and_keeps_explicit_you_independent() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, ControllerRef, SubAbilityLink};
    use engine::types::counter::CounterType;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let parsed = engine::parser::parse_oracle_text(
        ZURS_WEIRDING_ORACLE,
        "Zur's Weirding",
        &[],
        &["Enchantment".into()],
        &[],
    );
    assert!(parsed.parse_warnings.is_empty());
    assert_eq!(parsed.replacements.len(), 1);
    let mut replacement = parsed.replacements[0].clone();
    let mut reveal = *replacement.execute.take().unwrap();
    assert!(matches!(
        *reveal.effect,
        Effect::RevealTop {
            player: TargetFilter::PostReplacementDamageTarget,
            count: 1
        }
    ));
    let actor = reveal
        .instruction_actor
        .clone()
        .expect("actual they-reveal subject remains explicit");
    assert_eq!(
        actor.as_event(),
        Some(&TargetFilter::PostReplacementDamageTarget)
    );
    let counter = || {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )
    };
    let mut affected_counter = counter();
    affected_counter.instruction_actor = Some(actor);
    let mut you_counter = counter();
    you_counter.instruction_actor = Some(ControllerRef::You.into());
    you_counter.sub_link = SubAbilityLink::SequentialSibling;
    affected_counter.sub_ability = Some(Box::new(you_counter));
    reveal.sub_ability = Some(Box::new(affected_counter));
    replacement.execute = Some(Box::new(reveal));
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_enchantment_from_oracle(p3, "Synthetic counters after real Zur reveal", "")
        .controlled_by(P0)
        .with_replacement_definition(replacement)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Synthetic original draw",
            true,
            "Target player draws a card.",
        )
        .id();
    for player in [P0, P1, p2, p3] {
        scenario.with_library_top(player, &["First", "Second"]);
    }
    let mut runner = scenario.build();
    let original_card = *runner.state().players[p2.0 as usize]
        .library
        .front()
        .unwrap();
    let decoy = *runner.state().players[0].library.front().unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    let outcome = runner.cast(spell).target_player(p2).resolve();
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CardsRevealed { player, card_ids, .. } if *player == p2 && card_ids == &vec![original_card])).count(), 1);
    assert!(!outcome.events().iter().any(|event| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&decoy))));
    for actor in [P0, p2] {
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CounterAdded { actor: actual, object_id, counter_type: CounterType::Plus1Plus1, count: 1 } if *actual == actor && *object_id == host)).count(), 1);
    }
    outcome.assert_zone(&[original_card, decoy], Zone::Library);
    for player in [P0, P1, p2, p3] {
        outcome.assert_hand_drawn(player, 0);
    }
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 615.5 + CR 108.3: the owner who shuffles a prevented combat-damage
/// recipient is distinct from its controller and the prevention source.
#[test]
fn real_weeping_angel_owner_subject_survives_combat_and_performs_the_library_action() {
    use engine::game::combat::AttackTarget;
    use engine::types::ability::{AbilityDefinition, AbilityKind, SubAbilityLink};
    use engine::types::counter::CounterType;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let oracle = "Flash\nFirst strike, vigilance\nWhenever an opponent casts a creature spell, this creature isn't a creature until end of turn.\nIf this creature would deal combat damage to a creature, prevent that damage and that creature's owner shuffles it into their library.";
    let parsed = engine::parser::parse_oracle_text(
        oracle,
        "Weeping Angel",
        &[],
        &["Artifact".into(), "Creature".into()],
        &["Alien".into(), "Angel".into()],
    );
    assert!(parsed.parse_warnings.is_empty());
    assert_eq!(parsed.replacements.len(), 1);
    let mut replacement = parsed.replacements[0].clone();
    let mut printed = *replacement.execute.take().unwrap();
    let actor = printed
        .instruction_actor
        .clone()
        .expect("the real owner subject has a retained instruction actor");
    assert_eq!(
        actor.as_event(),
        Some(&TargetFilter::PostReplacementDamageTargetOwner)
    );
    let mut counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    );
    counter.instruction_actor = Some(actor);
    printed.sub_link = SubAbilityLink::SequentialSibling;
    counter.sub_ability = Some(Box::new(printed));
    replacement.execute = Some(Box::new(counter));
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let angel = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic counter then real Weeping Angel body",
            2,
            5,
            "First strike, vigilance",
        )
        .with_replacement_definition(replacement)
        .id();
    let victim = scenario
        .add_creature(p3, "Borrowed blocker", 1, 5)
        .controlled_by(p2)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut events = runner
        .declare_attackers(&[(angel, AttackTarget::Player(p2))])
        .unwrap()
        .events;
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DeclareBlockers { player, .. } if player == p2)
    );
    events.extend(runner.declare_blockers(&[(victim, angel)]).unwrap().events);
    let combat = runner.combat_damage();
    events.extend(combat.events().iter().cloned());
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::CounterAdded { actor, object_id, counter_type: CounterType::Plus1Plus1, count: 1 } if *actor == p3 && *object_id == angel)).count(), 1);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Library);
    assert!(runner.state().players[p3.0 as usize]
        .library
        .contains(&victim));
    assert!(!runner.state().players[p2.0 as usize]
        .library
        .contains(&victim));
    assert!(events.iter().any(|event| matches!(event, GameEvent::PlayerPerformedAction { player_id, action: engine::types::events::PlayerActionKind::ShuffledLibrary, .. } if *player_id == p3)));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 603.3 + CR 608.2c + CR 122.6: the event's targeter and the damaged
/// object's controller perform their own instructions. CounterAdded exposes
/// the performer independently of source, counter recipient, and card owner.
#[test]
fn actual_targeter_and_damage_recipient_triggers_keep_their_distinct_instruction_actors() {
    use engine::types::counter::CounterType;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for player_target in [false, true] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = if player_target {
            scenario.add_enchantment_from_oracle(p3, "Synthetic player-targeter actor",
                "Whenever you become the target of a spell or ability, that spell or ability's controller puts a +1/+1 counter on this enchantment.")
                .controlled_by(P0).id()
        } else {
            scenario.add_creature_from_oracle(p3, "Synthetic object-targeter actor", 2, 5,
                "Whenever this creature becomes the target of a spell or ability, that spell or ability's controller puts a +1/+1 counter on this creature.")
                .controlled_by(P0).id()
        };
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Synthetic targeter",
                true,
                if player_target {
                    "Target player gains 1 life."
                } else {
                    "Put a +1/+1 counter on target creature."
                },
            )
            .id();
        let mut runner = scenario.build();
        assert!(runner.state().objects[&source].parse_warnings.is_empty());
        assert_eq!(runner.state().objects[&source].trigger_definitions.len(), 1);
        runner.act(GameAction::PassPriority).unwrap();
        let cast = runner.cast(spell);
        let outcome = if player_target {
            cast.target_player(P0).resolve()
        } else {
            cast.target_object(source).resolve()
        };
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CounterAdded { actor: P1, object_id, counter_type: CounterType::Plus1Plus1, count: 1 } if *object_id == source)).count(), if player_target { 1 } else { 2 });
        assert!(!outcome.events().iter().any(|event| matches!(event, GameEvent::CounterAdded { actor, object_id, .. } if *object_id == source && *actor != P1)));
        assert_eq!(outcome.state().objects[&source].owner, p3);
        assert_eq!(outcome.state().objects[&source].controller, P0);
        assert!(outcome.state().stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Synthetic damaged-controller actor", 2, 5,
        "{T}: This creature deals 1 damage to target creature.\nWhenever this creature deals damage to a creature, that creature's controller puts a +1/+1 counter on that creature. That creature's owner puts a +1/+1 counter on that creature. You put a +1/+1 counter on this creature.").id();
    let victim = scenario
        .add_creature(p3, "Borrowed damaged object", 2, 5)
        .controlled_by(p2)
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().objects[&source].parse_warnings.is_empty());
    assert_eq!(runner.state().objects[&source].trigger_definitions.len(), 1);
    let outcome = runner.activate(source, 0).target_object(victim).resolve();
    for (actor, object) in [(p2, victim), (p3, victim), (P0, source)] {
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CounterAdded { actor: actual, object_id, counter_type: CounterType::Plus1Plus1, count: 1 } if *actual == actor && *object_id == object)).count(), 1);
    }
    assert_eq!(outcome.state().objects[&victim].damage_marked, 1);
    assert_eq!(
        outcome.state().objects[&victim]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2)
    );
    assert_eq!(
        outcome.state().objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 510.2 + CR 702.15e + CR 615.5: Phase A's source identities survive
/// the real lifelink replacement pause before Phase D dispatches a fresh rider.
#[test]
fn combat_lifelink_saved_choice_keeps_the_chosen_source_for_the_fresh_aggregate_rider() {
    use engine::game::combat::AttackTarget;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode,
    };
    use engine::types::replacements::ReplacementEvent;
    let p3 = PlayerId(3);
    for accept in [false, true] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let chosen = scenario
            .add_creature(p3, "Borrowed chosen damage source", 3, 5)
            .controlled_by(P0)
            .id();
        let lifelinker = scenario
            .add_creature(P0, "Independent lifelink dealer", 1, 5)
            .lifelink()
            .id();
        scenario
            .add_enchantment_from_oracle(P0, "Synthetic optional lifelink replacement", "")
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::GainLife)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
            );
        let shield_spell = scenario
            .add_spell_to_hand_from_oracle(P1, "New Way Forward", true, NEW_WAY_FORWARD_ORACLE)
            .id();
        scenario.with_library_top(P1, &["One", "Two", "Three", "Four"]);
        let mut runner = scenario.build();
        let chosen_pin = ObjectIdentityBinding::new(
            ObjectIncarnationRef::from_object(&runner.state().objects[&chosen]),
            Zone::Battlefield,
        );
        let lifelink_pin = ObjectIdentityBinding::new(
            ObjectIncarnationRef::from_object(&runner.state().objects[&lifelinker]),
            Zone::Battlefield,
        );
        runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        let cast = runner.cast(shield_spell).resolve();
        assert!(matches!(
            cast.final_waiting_for(),
            WaitingFor::DamageSourceChoice { player: P1, .. }
        ));
        runner
            .act(GameAction::ChooseDamageSource { source: chosen })
            .unwrap();
        assert!(runner.state().stack.is_empty());
        let installed = runner
            .state()
            .pending_damage_replacements
            .iter()
            .find(|shield| shield.runtime_execute.is_some())
            .unwrap();
        assert_eq!(
            installed.damage_source_filter,
            Some(TargetFilter::SpecificObject { id: chosen })
        );
        assert!(
            installed
                .runtime_execute
                .as_ref()
                .unwrap()
                .context
                .replacement_event
                .is_none(),
            "future installed work is unbound"
        );
        runner.advance_to_combat();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        ));
        let mut events = runner
            .declare_attackers(&[
                (chosen, AttackTarget::Player(P1)),
                (lifelinker, AttackTarget::Player(P1)),
            ])
            .unwrap()
            .events;
        let combat = runner.combat_damage();
        events.extend(combat.events().iter().cloned());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ));
        assert!(matches!(
            runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .proposed,
            engine::types::proposed_event::ProposedEvent::LifeGain {
                player_id: P0,
                amount: 1,
                ..
            }
        ));
        let tail = runner
            .state()
            .pending_combat_lifelink
            .as_ref()
            .expect("actual combat tail is parked before Phase D");
        assert_eq!(tail.source_identities.get(&chosen), Some(&chosen_pin));
        assert_eq!(tail.source_identities.get(&lifelinker), Some(&lifelink_pin));
        assert!(!tail.prevention_tally.is_empty());
        assert!(
            runner.state().players[1].hand.is_empty(),
            "the prevention rider has not fired before the lifelink answer"
        );
        let saved =
            serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_str::<PersistedGameState>(&saved)
            .unwrap()
            .into_game_state()
            .unwrap();
        let prepared = PersistedGameState::capture(runner.state().clone())
            .into_game_state()
            .unwrap();
        assert_restored_state(&restored, &prepared);
        for viewer in [P0, P1, PlayerId(2), p3] {
            let view = engine::game::visibility::filter_state_for_viewer(&restored, viewer);
            assert!(
                view.pending_combat_lifelink.is_none(),
                "the private combat continuation is not a viewer authority"
            );
        }
        runner = GameRunner::from_state(restored);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement {
                    index: usize::from(!accept),
                })
                .unwrap()
                .events,
        );
        for _ in 0..20 {
            if runner.state().pending_combat_lifelink.is_none()
                && runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "{:?}",
                runner.state().waiting_for
            );
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(runner.state().players[0].life, if accept { 17 } else { 18 });
        assert_eq!(runner.state().players[1].life, 19);
        assert_eq!(
            runner.state().players[1].hand.len(),
            3,
            "printed-you draws for P1, separately from the P0 source-controller recipient"
        );
        assert_eq!(runner.state().players[p3.0 as usize].life, 20);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(P0), amount: 3, is_combat: false, .. } if *source_id == shield_spell)).count(), 1);
        assert!(runner.state().pending_combat_lifelink.is_none());
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 113.7a + CR 608.2h: DamageEachPlayer is an independent production
/// dispatcher. Its sacrificed own source must reach the same exact-LKI authority.
#[test]
fn each_player_damage_keeps_the_pre_cost_source_through_a_real_prevention_followup() {
    use engine::types::ability::{
        DamageTargetFilter, DamageTargetPlayerScope, PlayerFilter, PreventionAmount,
        ReplacementDefinition,
    };
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let mogg = engine::parser::parse_oracle_text(
        MOGG_ORACLE,
        "Mogg Fanatic",
        &[],
        &["Creature".into()],
        &[],
    );
    let swans = engine::parser::parse_oracle_text(
        SWANS_ORACLE,
        "Swans of Bryn Argoll",
        &[],
        &["Creature".into()],
        &[],
    );
    assert!(mogg.parse_warnings.is_empty() && swans.parse_warnings.is_empty());
    let mut activation = mogg.abilities[0].clone();
    activation.effect = Box::new(Effect::DamageEachPlayer {
        amount: QuantityExpr::Fixed { value: 1 },
        player_filter: PlayerFilter::All,
    });
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Synthetic EachPlayer Mogg activation", 1, 1)
        .controlled_by(P1)
        .with_ability_definition(activation)
        .id();
    scenario
        .add_enchantment_from_oracle(p2, "Synthetic player shield with real Swans body", "")
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .prevention_shield(PreventionAmount::All)
                .damage_target_filter(DamageTargetFilter::Player {
                    player: DamageTargetPlayerScope::Specific(p2),
                })
                .execute(*swans.replacements[0].execute.clone().unwrap()),
        );
    for player in [P0, P1, p2, p3] {
        scenario.with_library_top(player, &["One", "Two"]);
    }
    let mut runner = scenario.build();
    let original = ObjectIdentityBinding::new(
        ObjectIncarnationRef::from_object(&runner.state().objects[&source]),
        Zone::Battlefield,
    );
    runner.act(GameAction::PassPriority).unwrap();
    let outcome = runner.activate(source, 0).resolve();
    outcome.assert_hand_drawn(P1, 1);
    for player in [P0, p2, p3] {
        outcome.assert_hand_drawn(player, 0);
    }
    for player in [P0, P1, p3] {
        outcome.assert_life_delta(player, -1);
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::DamageDealt { source_id, source_incarnation, target: TargetRef::Player(target), amount: 1, .. } if *source_id == source && *source_incarnation == Some(original.reference.incarnation) && *target == player)).count(), 1);
    }
    outcome.assert_life_delta(p2, 0);
    assert_eq!(outcome.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::DamagePrevented { source_id, target: TargetRef::Player(target), amount: 1 } if *source_id == source && *target == p2)).count(), 1);
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 608.2c + CR 615.5: recipient absence is independent of a valid actor.
/// Both ordinary damage dispatches must skip their dependent body and else,
/// retaining the independently sequenced tail exactly once.
#[test]
fn owned_missing_damage_recipients_skip_nonempty_decoys_after_nested_saved_dispatch() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, DamageTargetFilter, DamageTargetPlayerScope,
        DrawReplacementScope, EachDamageRecipient, PreventionAmount, ReplacementDefinition,
        ReplacementEventContext, ReplacementMode, SubAbilityLink,
    };
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for shared in [false, true] {
        for filter in [
            TargetFilter::PostReplacementSourceController,
            TargetFilter::PostReplacementDamageTargetOwner,
            TargetFilter::PostReplacementDamageSource,
            TargetFilter::PostReplacementDamageTarget,
        ] {
            for missing in [false, true] {
                let damage = |target| Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target,
                    damage_source: None,
                    excess: None,
                };
                let mut scenario = GameScenario::new_n_player(4, 42);
                scenario.at_phase(Phase::PreCombatMain);
                let outer = scenario
                    .add_creature(P1, "Synthetic original event dealer", 2, 10)
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Activated,
                        damage(TargetFilter::Any),
                    ))
                    .id();
                let shared_source = scenario
                    .add_creature(P0, "The eligible shared source", 2, 10)
                    .id();
                let decoy = scenario
                    .add_creature(p3, "Inherited object decoy", 2, 10)
                    .id();
                let host = scenario
                    .add_enchantment_from_oracle(P0, "Synthetic one-event prevention host", "")
                    .id();
                scenario.with_library_top(p3, &["Nested card", "Second"]);
                scenario
                    .add_enchantment_from_oracle(p3, "Synthetic nested draw choice", "")
                    .with_replacement_definition(
                        ReplacementDefinition::new(ReplacementEvent::Draw)
                            .draw_scope(DrawReplacementScope::IndividualDraw)
                            .mode(ReplacementMode::Optional { decline: None })
                            .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
                    );
                let mut runner = scenario.build();
                let pin = ObjectIdentityBinding::new(
                    ObjectIncarnationRef::from_object(&runner.state().objects[&outer]),
                    Zone::Battlefield,
                );
                assert!(runner.state().battlefield.contains(&shared_source));
                assert!(runner.state().objects[&shared_source]
                    .card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Creature));
                let effect = if shared {
                    Effect::EachSourceDealsDamage {
                        sources: TargetFilter::SpecificObject { id: shared_source },
                        amount: QuantityExpr::Fixed { value: 2 },
                        recipient: EachDamageRecipient::Shared(filter.clone()),
                    }
                } else {
                    damage(filter.clone())
                };
                let mut future = ResolvedAbility::new(
                    effect,
                    vec![TargetRef::Object(decoy), TargetRef::Player(p3)],
                    host,
                    P0,
                );
                future.instruction_actor = Some(ControllerRef::SpecificPlayer { id: P0 }.into());
                future.sub_link = SubAbilityLink::SequentialSibling;
                if missing {
                    future.context.replacement_event = Some(ReplacementEventContext {
                        source_id: (!matches!(filter, TargetFilter::PostReplacementDamageSource))
                            .then_some(outer),
                        source_identity: None,
                        event_target: matches!(
                            filter,
                            TargetFilter::PostReplacementSourceController
                                | TargetFilter::PostReplacementDamageSource
                        )
                        .then_some(TargetRef::Player(p2)),
                    });
                }
                let life = |value| {
                    ResolvedAbility::new(
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value },
                            player: TargetFilter::Controller,
                        },
                        vec![],
                        host,
                        P0,
                    )
                };
                future.else_ability = Some(Box::new(life(100)));
                let mut dependent = life(10);
                let mut tail = life(3);
                tail.sub_link = SubAbilityLink::SequentialSibling;
                dependent.sub_ability = Some(Box::new(tail));
                future.sub_ability = Some(Box::new(dependent));
                let mut head = ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::SpecificPlayer { id: p3 },
                    },
                    vec![],
                    host,
                    P0,
                );
                head.sub_ability = Some(Box::new(future));
                let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .damage_target_filter(DamageTargetFilter::Player {
                        player: DamageTargetPlayerScope::Specific(p2),
                    })
                    .runtime_execute(head);
                shield.consume_on_apply = true;
                install_turn_replacement(&mut runner, host, shield);
                runner.act(GameAction::PassPriority).unwrap();
                let outcome = runner.activate(outer, 0).target_player(p2).resolve();
                assert!(
                    matches!(outcome.final_waiting_for(), WaitingFor::ReplacementChoice { player, .. } if *player == p3)
                );
                let continued = runner
                    .state()
                    .resolution_stack
                    .iter()
                    .find_map(|frame| match frame {
                        engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                            Some(&frame.pending.chain)
                        }
                        _ => None,
                    })
                    .expect("future recipient instruction is genuinely parked");
                assert_eq!(
                    continued.instruction_actor,
                    Some(ControllerRef::SpecificPlayer { id: P0 }.into())
                );
                assert!(!continued.targets.is_empty());
                assert_eq!(
                    continued
                        .context
                        .replacement_event
                        .as_ref()
                        .unwrap()
                        .source_identity,
                    if missing { None } else { Some(pin) }
                );
                let saved =
                    serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
                        .unwrap();
                let restored = serde_json::from_str::<PersistedGameState>(&saved)
                    .unwrap()
                    .into_game_state()
                    .unwrap();
                let prepared = PersistedGameState::capture(runner.state().clone())
                    .into_game_state()
                    .unwrap();
                assert_restored_state(&restored, &prepared);
                runner = GameRunner::from_state(restored);
                let mut events = outcome.events().to_vec();
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap()
                        .events,
                );
                for _ in 0..16 {
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                        "{:?}",
                        runner.state().waiting_for
                    );
                    events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                }
                let recipient = match filter {
                    TargetFilter::PostReplacementSourceController => TargetRef::Player(P1),
                    TargetFilter::PostReplacementDamageTargetOwner
                    | TargetFilter::PostReplacementDamageTarget => TargetRef::Player(p2),
                    TargetFilter::PostReplacementDamageSource => TargetRef::Object(outer),
                    _ => unreachable!(),
                };
                let dealer = if shared { shared_source } else { host };
                assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamageDealt { source_id, target, amount: 2, .. } if *source_id == dealer && *target == recipient)).count(), usize::from(!missing));
                assert!(!events.iter().any(|event| matches!(event, GameEvent::DamageDealt { target: TargetRef::Object(id), .. } if *id == decoy)));
                assert_eq!(
                    runner.state().players[0].life,
                    if missing { 23 } else { 33 }
                );
                assert_eq!(
                    runner.state().players[1].life,
                    20 - if !missing && recipient == TargetRef::Player(P1) {
                        2
                    } else {
                        0
                    }
                );
                assert_eq!(
                    runner.state().players[p2.0 as usize].life,
                    20 - if !missing && recipient == TargetRef::Player(p2) {
                        2
                    } else {
                        0
                    }
                );
                assert_eq!(runner.state().players[p3.0 as usize].life, 20);
                assert_eq!(
                    runner.state().objects[&outer].damage_marked,
                    if !missing && recipient == TargetRef::Object(outer) {
                        2
                    } else {
                        0
                    }
                );
                assert_eq!(runner.state().objects[&decoy].damage_marked, 0);
                assert!(runner.state().stack.is_empty());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
            }
        }
    }
}

/// CR 101.4 + CR 608.2d + CR 614.6: the original drawer is excluded from
/// Zur's payer population even after a nested draw choice owns the resident
/// event. The parsed reveal/payment/owner/draw body remains intact.
#[test]
fn zurs_real_payer_population_and_hydration_keep_the_outer_drawer_after_a_saved_nested_draw() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DrawReplacementScope, ReplacementDefinition,
        ReplacementEventContext, ReplacementMode, SubAbilityLink,
    };
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for target_case in 0..3 {
        for pay in [false, true] {
            let parsed = engine::parser::parse_oracle_text(
                ZURS_WEIRDING_ORACLE,
                "Zur's Weirding",
                &[],
                &["Enchantment".into()],
                &[],
            );
            assert!(parsed.parse_warnings.is_empty());
            assert_eq!(parsed.replacements.len(), 1);
            let mut replacement = parsed.replacements[0].clone();
            let mut body = *replacement.execute.take().unwrap();
            assert!(matches!(
                *body.effect,
                Effect::RevealTop {
                    player: TargetFilter::PostReplacementDamageTarget,
                    count: 1
                }
            ));
            let mut pay_body = *body.sub_ability.take().unwrap();
            assert_eq!(
                pay_body.optional_for,
                Some(engine::types::ability::OpponentMayScope::AnyOtherPlayer)
            );
            pay_body.sub_link = SubAbilityLink::SequentialSibling;
            let mut nested = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SpecificPlayer { id: P1 },
                },
            );
            nested.sub_ability = Some(Box::new(pay_body));
            body.sub_ability = Some(Box::new(nested));
            let mut scenario = GameScenario::new_n_player(4, 42);
            scenario.at_phase(Phase::PreCombatMain);
            let host = scenario
                .add_enchantment_from_oracle(P0, "Synthetic nested Zur body", "")
                .id();
            let decoy = scenario
                .add_creature(p3, "Object is not a drawing player", 1, 5)
                .id();
            scenario
                .add_enchantment_from_oracle(P1, "Synthetic nested individual draw choice", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::Draw)
                        .draw_scope(DrawReplacementScope::IndividualDraw)
                        .mode(ReplacementMode::Optional { decline: None })
                        .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
                );
            let spell = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic draw source",
                    true,
                    "Target player draws a card.",
                )
                .id();
            scenario.with_library_top(P1, &["Nested card", "Nested second"]);
            let revealed = scenario.add_card_to_library_top(p2, "Outer revealed card");
            let mut runner = scenario.build();
            let mut runtime = build_resolved_from_def(&body, host, P0);
            let future = runtime
                .sub_ability
                .as_deref_mut()
                .unwrap()
                .sub_ability
                .as_deref_mut()
                .unwrap();
            if target_case != 0 {
                future.context.replacement_event = Some(ReplacementEventContext {
                    source_id: None,
                    source_identity: None,
                    event_target: (target_case == 2).then_some(TargetRef::Object(decoy)),
                });
            }
            let mut tail = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
                vec![],
                host,
                P0,
            );
            tail.sub_link = SubAbilityLink::SequentialSibling;
            let mut last = future;
            while let Some(ref mut sub) = last.sub_ability {
                last = sub;
            }
            last.sub_ability = Some(Box::new(tail));
            replacement.runtime_execute = Some(Box::new(runtime));
            install_turn_replacement(&mut runner, host, replacement);
            let outcome = runner.cast(spell).target_player(p2).resolve();
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::ReplacementChoice { player: P1, .. }
            ));
            assert!(matches!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .proposed,
                engine::types::proposed_event::ProposedEvent::Draw { player_id: P1, .. }
            ));
            let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
                .unwrap();
            let restored = serde_json::from_str::<PersistedGameState>(&saved)
                .unwrap()
                .into_game_state()
                .unwrap();
            let prepared = PersistedGameState::capture(runner.state().clone())
                .into_game_state()
                .unwrap();
            assert_restored_state(&restored, &prepared);
            runner = GameRunner::from_state(restored);
            let mut events = outcome.events().to_vec();
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap()
                    .events,
            );
            let mut offered = Vec::new();
            if target_case == 0 {
                let WaitingFor::OpponentMayChoice {
                    player, remaining, ..
                } = &runner.state().waiting_for
                else {
                    panic!(
                        "the original Zur instruction must offer the actual payer population: {:?}",
                        runner.state().waiting_for
                    );
                };
                let mut population = vec![*player];
                population.extend(remaining);
                assert_eq!(
                    population,
                    vec![P0, P1, p3],
                    "exclude outer drawer P2; nested drawer P1 is eligible"
                );
                let pending = &runner
                    .state()
                    .active_optional_effect_frame()
                    .unwrap()
                    .ability;
                assert!(pending.targets.contains(&TargetRef::Object(revealed)));
                assert!(pending.targets.contains(&TargetRef::Player(p2)));
                assert!(
                    !pending.targets.contains(&TargetRef::Player(P1)),
                    "hydration must not append the resident nested drawer"
                );
                assert_eq!(
                    pending
                        .context
                        .replacement_event
                        .as_ref()
                        .unwrap()
                        .event_target,
                    Some(TargetRef::Player(p2))
                );
                loop {
                    let WaitingFor::OpponentMayChoice { player, .. } = runner.state().waiting_for
                    else {
                        break;
                    };
                    offered.push(player);
                    events.extend(
                        runner
                            .act(GameAction::DecideOptionalEffect {
                                accept: pay && player == p3,
                            })
                            .unwrap()
                            .events,
                    );
                }
                assert_eq!(offered, vec![P0, P1, p3]);
            } else {
                assert!(
                    !matches!(
                        runner.state().waiting_for,
                        WaitingFor::OpponentMayChoice { .. }
                    ),
                    "missing/nonplayer owned drawer cannot borrow nested P1"
                );
            }
            for _ in 0..16 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                    "{:?}",
                    runner.state().waiting_for
                );
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            assert_eq!(runner.state().players[0].life, 23);
            assert_eq!(runner.state().players[1].life, 20);
            assert_eq!(runner.state().players[p2.0 as usize].life, 20);
            assert_eq!(
                runner.state().players[p3.0 as usize].life,
                20 - if pay && target_case == 0 { 2 } else { 0 }
            );
            assert_eq!(runner.state().players[1].hand.len(), 1);
            let destination = if target_case != 0 {
                Zone::Library
            } else if pay {
                Zone::Graveyard
            } else {
                Zone::Hand
            };
            assert_eq!(runner.state().objects[&revealed].zone, destination);
            assert_eq!(runner.state().objects[&revealed].owner, p2);
            assert_eq!(
                runner.state().players[p2.0 as usize]
                    .hand
                    .contains(&revealed),
                target_case == 0 && !pay
            );
            assert_eq!(
                runner.state().players[p2.0 as usize]
                    .graveyard
                    .contains(&revealed),
                target_case == 0 && pay
            );
            assert!(runner.state().players[0]
                .graveyard
                .iter()
                .all(|id| *id != revealed));
            assert!(runner.state().players[p3.0 as usize]
                .graveyard
                .iter()
                .all(|id| *id != revealed));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                    .count(),
                1
            );
            assert!(runner.state().stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}

/// CR 615.5 + CR 608.2h: Comeuppance's parsed source-type predicates and
/// reflection recipients read the original occurrence after a genuinely nested
/// damage replacement. The surrounding head and final life gain are synthetic.
#[test]
fn comeuppance_source_kind_keeps_the_outer_event_across_an_opposite_kind_saved_damage_choice() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, DamageTargetFilter, DamageTargetPlayerScope,
        PreventionAmount, ReplacementDefinition, ReplacementEventContext, ReplacementMode,
        SubAbilityLink,
    };
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for creature_outer in [false, true] {
        for (missing_outer, reenter) in [(false, false), (true, false), (false, true)] {
            for accept in [false, true] {
                let parsed = engine::parser::parse_oracle_text(
                    COMEUPPANCE_ORACLE,
                    "Comeuppance",
                    &[],
                    &["Instant".into()],
                    &[],
                );
                assert!(parsed.parse_warnings.is_empty());
                let rider = parsed.abilities[0].sub_ability.as_ref().unwrap();
                assert!(matches!(rider.condition, Some(engine::types::ability::AbilityCondition::PostReplacementDamageSourceMatchesFilter { .. })));
                let damage = || Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                };
                let mut scenario = GameScenario::new_n_player(4, 42);
                scenario.at_phase(Phase::PreCombatMain);
                let outer = if creature_outer {
                    scenario
                        .add_creature(p3, "Synthetic creature outer dealer", 2, 5)
                        .controlled_by(P1)
                        .with_ability_definition(AbilityDefinition::new(
                            AbilityKind::Activated,
                            damage(),
                        ))
                        .id()
                } else {
                    scenario
                        .add_artifact_from_oracle(p3, "Synthetic noncreature outer dealer", "")
                        .controlled_by(P1)
                        .with_ability_definition(AbilityDefinition::new(
                            AbilityKind::Activated,
                            damage(),
                        ))
                        .id()
                };
                let child = if creature_outer {
                    scenario
                        .add_artifact_from_oracle(p2, "Synthetic noncreature child dealer", "")
                        .id()
                } else {
                    scenario
                        .add_creature(p2, "Synthetic creature child dealer", 2, 5)
                        .id()
                };
                let host = scenario
                    .add_enchantment_from_oracle(P0, "Synthetic Comeuppance rider host", "")
                    .id();
                scenario
                    .add_enchantment_from_oracle(p3, "Synthetic nested damage choice", "")
                    .with_replacement_definition(
                        ReplacementDefinition::new(ReplacementEvent::DamageDone)
                            .prevention_shield(PreventionAmount::All)
                            .damage_target_filter(DamageTargetFilter::Player {
                                player: DamageTargetPlayerScope::Specific(p3),
                            })
                            .mode(ReplacementMode::Optional { decline: None })
                            .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
                    );
                let mut runner = scenario.build();
                let original = ObjectIdentityBinding::new(
                    ObjectIncarnationRef::from_object(&runner.state().objects[&outer]),
                    Zone::Battlefield,
                );
                let child_pin = ObjectIdentityBinding::new(
                    ObjectIncarnationRef::from_object(&runner.state().objects[&child]),
                    Zone::Battlefield,
                );
                let mut future = build_resolved_from_def(rider, host, P0);
                if missing_outer {
                    future.context.replacement_event = Some(ReplacementEventContext {
                        source_id: None,
                        source_identity: None,
                        event_target: Some(TargetRef::Player(P0)),
                    });
                }
                let mut tail = ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                        player: TargetFilter::Controller,
                    },
                    vec![],
                    host,
                    P0,
                );
                tail.sub_link = SubAbilityLink::SequentialSibling;
                let mut last = &mut future;
                while let Some(ref mut sub) = last.sub_ability {
                    last = sub;
                }
                last.sub_ability = Some(Box::new(tail));
                let mut head =
                    ResolvedAbility::new(damage(), vec![TargetRef::Player(p3)], child, p2);
                head.context.source_identity = Some(child_pin);
                head.source_incarnation = Some(child_pin.reference.incarnation);
                head.instruction_actor = Some(ControllerRef::You.into());
                head.sub_ability = Some(Box::new(future));
                if reenter {
                    // CR 400.7 + CR 608.2h: real zone moves produce a new
                    // incarnation, then a copy effect gives that new object
                    // the opposite type. Exact original LKI still governs.
                    let move_effect = |origin, destination| Effect::ChangeZone {
                        origin: Some(origin),
                        destination,
                        target: TargetFilter::SpecificObject { id: outer },
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    };
                    let mut copy = ResolvedAbility::new(
                        Effect::BecomeCopy {
                            target: TargetFilter::SpecificObject { id: child },
                            recipient: engine::types::ability::CopyRecipient::Untargeted(
                                TargetFilter::SpecificObject { id: outer },
                            ),
                            duration: None,
                            mana_value_limit: None,
                            additional_modifications: vec![],
                        },
                        vec![TargetRef::Object(child)],
                        host,
                        P0,
                    );
                    copy.sub_ability = Some(Box::new(head));
                    let mut arrive = ResolvedAbility::new(
                        move_effect(Zone::Exile, Zone::Battlefield),
                        vec![TargetRef::Object(outer)],
                        host,
                        P0,
                    );
                    arrive.sub_ability = Some(Box::new(copy));
                    head = ResolvedAbility::new(
                        move_effect(Zone::Battlefield, Zone::Exile),
                        vec![TargetRef::Object(outer)],
                        host,
                        P0,
                    );
                    head.sub_ability = Some(Box::new(arrive));
                }
                let shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .damage_target_filter(DamageTargetFilter::Player {
                        player: DamageTargetPlayerScope::Specific(P0),
                    })
                    .runtime_execute(head);
                // Fixture construction installs a typed surrounding replacement;
                // the reflected body and both source-type gates are the real
                // Comeuppance parser/owner output, not recreated conditions.
                install_turn_replacement(&mut runner, host, shield);
                runner.act(GameAction::PassPriority).unwrap();
                let outcome = runner.activate(outer, 0).target_player(P0).resolve();
                assert!(
                    matches!(outcome.final_waiting_for(), WaitingFor::ReplacementChoice { player, .. } if *player == p3)
                );
                let pending = runner.state().pending_replacement.as_ref().unwrap();
                assert!(
                    matches!(&pending.proposed, engine::types::proposed_event::ProposedEvent::Damage { source_id, source_identity, target: TargetRef::Player(player), amount: 1, .. } if *source_id == child && *source_identity == Some(child_pin) && *player == p3)
                );
                let continued = runner
                    .state()
                    .resolution_stack
                    .iter()
                    .find_map(|frame| match frame {
                        engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                            Some(&frame.pending.chain)
                        }
                        _ => None,
                    })
                    .expect("outer conditional rider is genuinely parked");
                if reenter {
                    let returned = &runner.state().objects[&outer];
                    assert_eq!(returned.zone, Zone::Battlefield);
                    assert_ne!(returned.incarnation, original.reference.incarnation);
                    assert_eq!(returned.controller, p3);
                    assert_eq!(
                        returned
                            .card_types
                            .core_types
                            .contains(&engine::types::card_type::CoreType::Creature),
                        !creature_outer
                    );
                    let lki =
                        &runner.state().lki_by_incarnation[&outer][&original.reference.incarnation];
                    assert_eq!(lki.controller, P1);
                    assert_eq!(
                        lki.card_types
                            .contains(&engine::types::card_type::CoreType::Creature),
                        creature_outer
                    );
                }
                let owned = continued.context.replacement_event.as_ref().unwrap();
                assert_eq!(
                    owned.source_identity,
                    if missing_outer { None } else { Some(original) }
                );
                let saved =
                    serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
                        .unwrap();
                let restored = serde_json::from_str::<PersistedGameState>(&saved)
                    .unwrap()
                    .into_game_state()
                    .unwrap();
                let prepared = PersistedGameState::capture(runner.state().clone())
                    .into_game_state()
                    .unwrap();
                assert_restored_state(&restored, &prepared);
                runner = GameRunner::from_state(restored);
                let mut events = outcome.events().to_vec();
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement {
                            index: usize::from(!accept),
                        })
                        .unwrap()
                        .events,
                );
                for _ in 0..16 {
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                        "{:?}",
                        runner.state().waiting_for
                    );
                    events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                }
                assert_eq!(
                    runner.state().players[0].life,
                    23,
                    "outer damage is prevented; independent tail runs exactly once"
                );
                assert_eq!(
                    runner.state().players[1].life,
                    20 - i32::from(!creature_outer && !missing_outer)
                );
                assert_eq!(
                    runner.state().players[p2.0 as usize].life,
                    20,
                    "child controller never receives outer reflection"
                );
                assert_eq!(
                    runner.state().players[p3.0 as usize].life,
                    20 - i32::from(!accept)
                );
                if creature_outer {
                    assert_eq!(
                        runner.state().objects[&outer].damage_marked,
                        u32::from(!missing_outer && !reenter)
                    );
                }
                assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamageDealt { source_id, amount: 1, target, .. }
                    if *source_id == host && *target == if creature_outer { TargetRef::Object(outer) } else { TargetRef::Player(P1) }
                )).count(), usize::from(!missing_outer && !(creature_outer && reenter)));
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                        .count(),
                    1
                );
                assert!(runner.state().stack.is_empty());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
            }
        }
    }
}

/// CR 109.4 + CR 113.7a + CR 615.5: a discarded Channel source remains
/// the dealer and its owner draws. This is a real hand activation, rather
/// than a battlefield-source shortcut or a fabricated replacement drain.
#[test]
fn channel_damage_to_swans_uses_the_discarded_hand_occurrences_owner() {
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let swans = scenario
        .add_creature_from_oracle(p3, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
        .controlled_by(p2)
        .id();
    let source = scenario
        .add_creature_to_hand_from_oracle(P1, "Ghost-Lit Raider", 2, 1, GHOST_LIT_RAIDER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); 4],
    );
    for player in [P0, P1, p2, p3] {
        scenario.with_library_top(player, &["First", "Second", "Third", "Fourth", "Fifth"]);
    }
    let mut runner = scenario.build();
    let original = ObjectIdentityBinding::new(
        ObjectIncarnationRef::from_object(&runner.state().objects[&source]),
        Zone::Hand,
    );
    assert!(runner.state().objects[&source].parse_warnings.is_empty());
    assert_eq!(runner.state().objects[&source].abilities.len(), 2);
    assert_eq!(
        runner.state().objects[&source].abilities[1].activation_zone,
        Some(Zone::Hand)
    );
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let outcome = runner.activate(source, 1).target_object(swans).resolve();
    outcome.assert_hand_drawn(P1, 4);
    for player in [P0, p2, p3] {
        outcome.assert_hand_drawn(player, 0);
    }
    assert_eq!(outcome.state().objects[&source].zone, Zone::Graveyard);
    assert_ne!(
        outcome.state().objects[&source].incarnation,
        original.reference.incarnation
    );
    let departure = outcome
        .state()
        .zone_changes_this_turn
        .iter()
        .find_map(|record| {
            record
                .trigger_source_context
                .as_ref()
                .filter(|context| context.identity == original)
        })
        .expect("the discard cost records the original hand occurrence");
    assert_eq!(departure.lki.owner, P1);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event,
        GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 4 }
            if *source_id == source && *target == swans
    )).count(), 1);
    assert_eq!(outcome.state().objects[&swans].damage_marked, 0);
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 113.7a + CR 608.2h + CR 615.5: the source already left the battlefield
/// when Swans prevents its damage; the borrowed source's last controller draws.
#[test]
fn printed_sacrificed_mogg_and_bloodfire_swans_draw_for_the_battlefield_controller() {
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for (name, oracle, amount) in [
        ("Mogg Fanatic", MOGG_ORACLE, 1),
        ("Bloodfire Colossus", BLOODFIRE_ORACLE, 6),
    ] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let swans = scenario
            .add_creature_from_oracle(p3, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
            .controlled_by(p2)
            .id();
        let source = scenario
            .add_creature_from_oracle(P0, name, amount as i32, amount as i32, oracle)
            .controlled_by(P1)
            .id();
        scenario.with_mana_pool(
            P1,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );
        for player in [P0, P1, p2, p3] {
            scenario.with_library_top(
                player,
                &[
                    "First", "Second", "Third", "Fourth", "Fifth", "Sixth", "Seventh",
                ],
            );
        }
        let mut runner = scenario.build();
        let original = ObjectIdentityBinding::new(
            ObjectIncarnationRef::from_object(&runner.state().objects[&source]),
            Zone::Battlefield,
        );
        assert_eq!(
            (
                runner.state().objects[&source].owner,
                runner.state().objects[&source].controller
            ),
            (P0, P1)
        );
        assert!(runner.state().objects[&source].parse_warnings.is_empty());
        assert!(runner.state().objects[&swans].parse_warnings.is_empty());
        assert_eq!(
            runner.state().objects[&swans].replacement_definitions.len(),
            1
        );
        let activation = &runner.state().objects[&source].abilities[0];
        assert_eq!(
            activation.kind,
            engine::types::ability::AbilityKind::Activated
        );
        assert!(activation.cost.is_some());
        if amount == 1 {
            assert!(matches!(
                *activation.effect,
                Effect::DealDamage {
                    damage_source: None,
                    ..
                }
            ));
        } else {
            assert!(matches!(*activation.effect, Effect::DamageAll { .. }));
        }
        runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        let outcome = if amount == 1 {
            runner.activate(source, 0).target_object(swans).resolve()
        } else {
            runner.activate(source, 0).resolve()
        };
        outcome.assert_hand_drawn(P1, amount as i64);
        for other in [P0, p2, p3] {
            outcome.assert_hand_drawn(other, 0);
        }
        assert_eq!(outcome.state().objects[&source].zone, Zone::Graveyard);
        assert_ne!(
            outcome.state().objects[&source].incarnation,
            original.reference.incarnation
        );
        assert_eq!(outcome.state().objects[&swans].zone, Zone::Battlefield);
        assert_eq!(outcome.state().objects[&swans].damage_marked, 0);
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: prevented }
                if *source_id == source && *target == swans && *prevented == amount
        )).count(), 1, "the actual prevention branch must execute exactly once");
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::PermanentSacrificed { object_id, player_id: P1 } if *object_id == source
        )).count(), 1);
        if amount == 6 {
            for player in [P0, P1, p2, p3] {
                outcome.assert_life_delta(player, -6);
                assert_eq!(outcome.events().iter().filter(|event| matches!(event,
                    GameEvent::DamageDealt { source_id, source_incarnation, target: TargetRef::Player(target), amount: 6, .. }
                        if *source_id == source && *target == player && *source_incarnation == Some(original.reference.incarnation)
                )).count(), 1);
            }
        }
        assert!(outcome.state().stack.is_empty());
        assert!(matches!(
            outcome.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 615.5 + CR 608.2h: a future outer instruction owns its event across an
/// actual nested replacement choice and canonical save/restore. The extra draw
/// is a synthetic body; its source-controller instruction is Swans's parsed body.
#[test]
fn template_and_fresh_resolved_source_followups_keep_the_outer_event_after_nested_saved_choice() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DrawReplacementScope, ReplacementDefinition,
        ReplacementMode, SubAbilityLink,
    };
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for fresh_resolved in [false, true] {
        for accept in [false, true] {
            let parsed = engine::parser::parse_oracle_text(
                SWANS_ORACLE,
                "Swans of Bryn Argoll",
                &[],
                &["Creature".into()],
                &[],
            );
            assert!(parsed.parse_warnings.is_empty());
            assert_eq!(parsed.replacements.len(), 1);
            let mut shield = parsed.replacements[0].clone();
            let mut future = *shield.execute.take().unwrap();
            future.sub_link = SubAbilityLink::SequentialSibling;
            let mut head = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SpecificPlayer { id: p3 },
                },
            );
            head.instruction_actor = Some(engine::types::ability::ControllerRef::You.into());
            head.sub_ability = Some(Box::new(future));
            let mut scenario = GameScenario::new_n_player(4, 42);
            scenario.at_phase(Phase::PreCombatMain);
            let swans;
            {
                let mut host = scenario.add_creature(p2, "Synthetic nested Swans body", 4, 3);
                swans = host.id();
                if fresh_resolved {
                    let runtime = build_resolved_from_def(&head, swans, p2);
                    assert!(runtime.context.replacement_event.is_none());
                    assert!(runtime
                        .sub_ability
                        .as_ref()
                        .unwrap()
                        .context
                        .replacement_event
                        .is_none());
                    shield.runtime_execute = Some(Box::new(runtime));
                } else {
                    shield.execute = Some(Box::new(head));
                }
                host.with_replacement_definition(shield);
            }
            let nested = ReplacementDefinition::new(ReplacementEvent::Draw)
                .draw_scope(DrawReplacementScope::IndividualDraw)
                .mode(ReplacementMode::Optional { decline: None })
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
            scenario
                .add_enchantment_from_oracle(p3, "Synthetic nested draw choice", "")
                .with_replacement_definition(nested);
            let source = scenario
                .add_creature_from_oracle(P0, "Mogg Fanatic", 1, 1, MOGG_ORACLE)
                .controlled_by(P1)
                .id();
            for player in [P0, P1, p2, p3] {
                scenario.with_library_top(player, &["First", "Second", "Third"]);
            }
            let mut runner = scenario.build();
            let original = ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&runner.state().objects[&source]),
                Zone::Battlefield,
            );
            runner.act(GameAction::PassPriority).unwrap();
            let outcome = runner.activate(source, 0).target_object(swans).resolve();
            assert!(
                matches!(outcome.state().waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == p3)
            );
            let pending = runner.state().pending_replacement.as_ref().unwrap();
            assert!(pending.is_optional);
            assert!(
                matches!(pending.proposed, engine::types::proposed_event::ProposedEvent::Draw { player_id, .. } if player_id == p3)
            );
            let continued = runner
                .state()
                .resolution_stack
                .iter()
                .find_map(|frame| match frame {
                    engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                        Some(&frame.pending.chain)
                    }
                    _ => None,
                })
                .expect("the outer future instruction is genuinely parked");
            let owned = continued
                .context
                .replacement_event
                .as_ref()
                .expect("future node owns the outer event");
            assert_eq!(owned.source_id, Some(source));
            assert_eq!(owned.source_identity, Some(original));
            assert_eq!(owned.event_target, Some(TargetRef::Object(swans)));
            let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
                .unwrap();
            let restored = serde_json::from_str::<PersistedGameState>(&saved)
                .unwrap()
                .into_game_state()
                .unwrap();
            let prepared = PersistedGameState::capture(runner.state().clone())
                .into_game_state()
                .unwrap();
            assert_restored_state(&restored, &prepared);
            runner = GameRunner::from_state(restored);
            let mut events = outcome.events().to_vec();
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement {
                        index: usize::from(!accept),
                    })
                    .unwrap()
                    .events,
            );
            for _ in 0..16 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                    "unexpected continuation {:?}",
                    runner.state().waiting_for
                );
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            assert_eq!(
                runner.state().players[P1.0 as usize].hand.len(),
                1,
                "outer source-controller draw is P1, never nested drawer P3"
            );
            assert_eq!(
                runner.state().players[p3.0 as usize].hand.len(),
                usize::from(!accept)
            );
            for player in [P0, p2] {
                assert!(runner.state().players[player.0 as usize].hand.is_empty());
            }
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 1 } if *source_id == source && *target == swans)).count(), 1);
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&swans].zone, Zone::Battlefield);
            assert!(runner.state().stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}
