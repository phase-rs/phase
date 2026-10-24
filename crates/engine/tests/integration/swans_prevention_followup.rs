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
    DrainStatus, GameState, PostReplacementDrain, ResidentDrainPolicy,
};
use engine::types::identifiers::{CardId, ObjectId, ObjectIdentityBinding, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

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
            .add_creature_from_oracle(p2, "Swans of Bryn Argoll", 4, 3, SWANS_ORACLE)
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
            let saved = serde_json::to_string(runner.state()).unwrap();
            let restored: GameState = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored, *runner.state());
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
