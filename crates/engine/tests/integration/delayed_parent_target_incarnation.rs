//! CR 400.7 / CR 603.7c — a delayed triggered ability's `ParentTarget` referent
//! is pinned to the incarnation it had when the trigger was created.
//!
//! CR 603.7c: "A delayed triggered ability that refers to a particular object
//! still affects it even if the object changes characteristics. However, if that
//! object is no longer in the zone it's expected to be in at the time the
//! delayed triggered ability resolves, the ability won't affect it. (Note that
//! if that object left that zone and then returned, it's a new object and thus
//! won't be affected. See rule 400.7.)"
//!
//! The driver bug: Goryo's Vengeance reanimates a creature and schedules
//! "Exile it at the beginning of the next end step". Blinking that creature with
//! Ephemerate makes it a NEW object (CR 400.7) that merely reuses the same
//! storage `ObjectId`, so the delayed trigger must no longer affect it.
//!
//! Controls in this file assert the OTHER direction of CR 603.7c's operative
//! test: a delayed trigger whose own condition IS the referent's zone change
//! (Saffi Eriksdotter's "when that creature dies", Lagrella's "when an exiled
//! card enters") expects the referent to have moved, and must keep working.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    DelayedTriggerCondition, Effect, TargetFilter, TargetRef, TypeFilter, WheneverEventExpiry,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

// ---------------------------------------------------------------- Oracle text
// Verbatim Oracle text (Scryfall). A paraphrase can take a different parser
// branch and go green while the real card stays broken.

const GORYOS_VENGEANCE: &str = "Return target legendary creature card from your graveyard to the battlefield. That creature gains haste. Exile it at the beginning of the next end step.";

const EPHEMERATE: &str =
    "Exile target creature you control, then return it to the battlefield under its owner's control.";

const SAFFI_ERIKSDOTTER: &str = "Sacrifice Saffi Eriksdotter: When target creature is put into your graveyard this turn, return that card to the battlefield.";

const LAGRELLA: &str = "When Lagrella enters, exile any number of other target creatures controlled by different players until Lagrella leaves the battlefield. When an exiled card enters under your control this way, put two +1/+1 counters on it.";

/// A plain removal spell used to move a referent to the graveyard through the
/// real cast pipeline rather than by mutating state behind the engine's back.
const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

const TOGETHER_FOREVER: &str = "When this enchantment enters, support 2. (Put a +1/+1 counter on each of up to two target creatures.)\n{1}: Choose target creature with a counter on it. When that creature dies this turn, return that card to its owner's hand.";
const VAMPIRE_HEXMAGE: &str =
    "First strike\nSacrifice this creature: Remove all counters from target permanent.";
const A_GOOD_DAY_TO_DIE: &str = "Target creature gets +2/+0 until end of turn. When that creature dies this turn, you draw a card.";
const CRYPTEK: &str = "{1}{B}, {T}: Choose another target artifact creature you control. When that creature dies this turn, return it to the battlefield tapped under your control.";
const FATAL_FISSURE: &str = "Choose target creature. When that creature dies this turn, you earthbend 4. (Target land you control becomes a 0/0 creature with haste that's still a land. Put four +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
const TIME_TO_FEED: &str = "Choose target creature an opponent controls. When that creature dies this turn, you gain 3 life. Target creature you control fights that creature. (Each deals damage equal to its power to the other.)";

#[test]
fn together_forever_does_not_install_when_its_target_loses_its_last_counter_in_response() {
    use engine::types::counter::CounterType;
    use engine::types::game_state::CastPaymentMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    scenario.with_mana_pool(P0, mana(8));
    let source = scenario
        .add_enchantment_from_oracle(P0, "Bounded return", TOGETHER_FOREVER)
        .id();
    let hexmage = scenario
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    let victim = scenario
        .add_creature(P0, "Creature with its last counter", 2, 2)
        .id();
    let alternative = scenario
        .add_creature(P0, "Other countered creature", 2, 2)
        .id();
    scenario.with_counter(victim, CounterType::Plus1Plus1, 1);
    scenario.with_counter(alternative, CounterType::Plus1Plus1, 1);
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let occurrence = runner.state().objects[&victim].incarnation;
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce the return ability");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("the counter makes this target legal");
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay for the activation");
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, source);
    runner.activate(hexmage, 0).target_object(victim).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&victim].incarnation, occurrence);
    assert!(runner.state().objects[&victim].counters.is_empty());
    assert!(runner.state().delayed_triggers.is_empty());
    let death = runner.cast(removal).target_object(victim).resolve();
    assert_eq!(death.zone_of(victim), Zone::Graveyard);
    assert!(death.state().stack.is_empty());
    assert!(matches!(
        death.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn a_good_day_to_die_does_not_install_after_its_only_target_dies_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let victim = scenario
        .add_creature(P1, "Initially legal target", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Printed draw watcher", true, A_GOOD_DAY_TO_DIE)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let announced = runner.cast(spell).target_object(victim).commit();
    assert_eq!(
        announced
            .state()
            .stack
            .last()
            .and_then(|entry| entry.ability())
            .expect("announced spell")
            .targets,
        [TargetRef::Object(victim)],
    );
    let result = runner.cast(removal).target_object(victim).resolve();
    assert_eq!(result.zone_of(victim), Zone::Graveyard);
    assert_eq!(result.zone_of(spell), Zone::Graveyard);
    assert_eq!(result.hand_drawn(P0), 0);
    assert!(result.state().delayed_triggers.is_empty());
    assert!(result.state().stack.is_empty());
    assert!(matches!(
        result.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn together_forever_queues_on_a_token_death_after_the_token_ceases_to_exist() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    scenario.with_mana_pool(P0, mana(8));
    let create = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Printed Soldier maker",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();
    let support = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Bounded return", TOGETHER_FOREVER)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let created = runner.cast(create).resolve();
    let mut tokens: Vec<_> = created
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token)
        .map(|obj| obj.id)
        .collect();
    tokens.sort_unstable();
    let [token, other] = tokens.as_slice() else {
        panic!("the printed instant creates two Soldiers");
    };
    let supported = runner
        .cast(support)
        .target_objects(&[*token, *other])
        .resolve();
    assert_eq!(supported.zone_of(support), Zone::Battlefield);
    assert!(!supported.state().objects[token].counters.is_empty());
    runner.activate(support, 0).target_object(*token).resolve();
    let [watcher] = runner.state().delayed_triggers.as_slice() else {
        panic!("the activation must install one watcher");
    };
    assert_eq!(
        watcher
            .ability
            .context
            .delayed_watch_incarnation
            .expect("token watch pin")
            .object_id,
        *token
    );
    runner.cast(removal).target_object(*token).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("pass to removal");
    let death = runner
        .act(GameAction::PassPriority)
        .expect("resolve removal");
    assert!(death.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
            if object_id == token
    )));
    assert!(!runner.state().objects.contains_key(token));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, support);
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects.contains_key(token));
    assert!(!runner.state().players[0].hand.contains(token));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn watchers_expire_even_when_their_printed_payload_has_a_separate_gap() {
    exercise_watchers_with_existing_payload_gaps(None);
}

#[test]
fn heroic_sacrifice_fires_its_existing_counter_payload_this_turn() {
    exercise_watchers_with_existing_payload_gaps(Some("Heroic Sacrifice"));
}

#[test]
fn reincarnation_fires_its_existing_graveyard_choice_this_turn() {
    exercise_watchers_with_existing_payload_gaps(Some("Printed reanimation watcher"));
}

#[test]
fn rushed_rebirth_fires_its_existing_search_payload_this_turn() {
    exercise_watchers_with_existing_payload_gaps(Some("Printed search watcher"));
}

#[test]
fn end_blaze_epiphany_fires_its_existing_exile_payload_this_turn() {
    exercise_watchers_with_existing_payload_gaps(Some("End-Blaze Epiphany"));
}

const HEROIC_SACRIFICE_WATCH: &str = concat!(
    "Choose target creature you control. Until end of turn, all damage that would be dealt to ",
    "you and creatures you control is dealt to the chosen creature instead (if it's still on ",
    "the battlefield). When that creature dies this turn, put its counters on up to one target ",
    "creature you control and draw a card.",
);
const REINCARNATION_WATCH: &str = concat!(
    "Choose target creature. When that creature dies this turn, return a creature card from ",
    "its owner's graveyard to the battlefield under the control of that creature's owner.",
);
const RUSHED_REBIRTH_WATCH: &str = concat!(
    "Choose target creature. When that creature dies this turn, search your library for a ",
    "creature card with lesser mana value, put it onto the battlefield tapped, then shuffle.",
);
const END_BLAZE_EPIPHANY_WATCH: &str = concat!(
    "End-Blaze Epiphany deals X damage to target creature. When that creature dies this turn, ",
    "exile a number of cards from the top of your library equal to its power, then choose a ",
    "card exiled this way. Until the end of your next turn, you may play that card.",
);

fn exercise_watchers_with_existing_payload_gaps(live_card: Option<&str>) {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::CastingPermission;
    use engine::types::counter::CounterType;
    use engine::types::mana::{ManaCost, ManaCostShard};

    for (name, oracle, x, immediate_draw) in [
        ("Heroic Sacrifice", HEROIC_SACRIFICE_WATCH, None, 1),
        ("Printed reanimation watcher", REINCARNATION_WATCH, None, 0),
        ("Printed search watcher", RUSHED_REBIRTH_WATCH, None, 0),
        ("End-Blaze Epiphany", END_BLAZE_EPIPHANY_WATCH, Some(1), 0),
    ] {
        if live_card.is_some_and(|card| card != name) {
            continue;
        }
        let late = live_card.is_none();
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana(4));
        stock_libraries(&mut scenario);
        let victim = scenario
            .add_creature(
                if name == "Printed reanimation watcher" {
                    P1
                } else {
                    P0
                },
                "Watched creature",
                2,
                4,
            )
            .controlled_by(P0)
            .with_mana_cost(ManaCost::generic(4))
            .id();
        scenario.with_counter(victim, CounterType::Plus1Plus1, 1);
        let recipient = scenario.add_creature(P0, "Counter recipient", 2, 2).id();
        let other_recipient = scenario
            .add_creature(P0, "Other counter recipient", 2, 2)
            .id();
        let caster_grave = scenario
            .add_creature_to_graveyard(P0, "Caster's graveyard card", 1, 1)
            .id();
        let owner_grave = scenario
            .add_creature_to_graveyard(P1, "Owner's graveyard card", 1, 1)
            .id();
        let unrelated_exile = scenario
            .add_land_to_exile(P0, "Previously exiled land")
            .id();
        let top = scenario.add_card_to_library_top(P0, "Top card");
        let lesser = (name == "Printed search watcher").then(|| {
            let db = engine::database::card_db::CardDatabase::from_integration_fixture_typing_faces_on_read()
                .expect("integration card fixture must load");
            scenario.add_real_card(P0, "Llanowar Elves", Zone::Library, &db)
        });
        let mut spell = scenario.add_spell_to_hand_from_oracle(P0, name, true, oracle);
        if x.is_some() {
            spell.with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            });
        }
        let spell = spell.id();
        let removal = scenario
            .add_spell_to_hand_from_oracle(P1, "Murder", true, DESTROY_TARGET_CREATURE)
            .id();
        let mut runner = scenario.build();
        let announced = runner.state().objects[&victim].incarnation;
        let cast = runner.cast(spell).target_object(victim);
        let outcome = if let Some(x) = x {
            cast.x(x).resolve()
        } else {
            cast.resolve()
        };
        assert_eq!(outcome.zone_of(victim), Zone::Battlefield);
        assert_eq!(outcome.hand_drawn(P0), immediate_draw);
        let hand_after_install = outcome.state().players[0].hand.len();
        let [watcher] = outcome.state().delayed_triggers.as_slice() else {
            panic!("{name} must install exactly one watcher");
        };
        let pin = watcher
            .ability
            .context
            .delayed_watch_incarnation
            .expect("announced watch pin");
        assert_eq!((pin.object_id, pin.incarnation), (victim, announced));
        assert!(matches!(
            &watcher.condition,
            DelayedTriggerCondition::WheneverEvent {
                expiry: WheneverEventExpiry::EndOfTurn,
                ..
            }
        ));
        if late {
            advance_past_end_of_turn(&mut runner);
            assert!(runner.state().delayed_triggers.is_empty());
            assert_eq!(runner.state().objects[&victim].incarnation, announced);
        } else {
            runner
                .act(GameAction::PassPriority)
                .expect("give P1 priority");
        }
        if !late && name == "Heroic Sacrifice" {
            runner.cast(removal).target_object(victim).commit();
            runner
                .act(GameAction::PassPriority)
                .expect("pass to Murder");
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Murder");
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
            // Existing payload defects: the draw was already resolved above, and
            // the counter recipient is required despite the printed "up to one".
            let WaitingFor::TriggerTargetSelection {
                player: P0,
                target_slots,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "expected Heroic Sacrifice's counter target, got {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(target_slots[0].legal_targets.len(), 2);
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(recipient)));
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(other_recipient)));
            assert!(!target_slots[0].optional);
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(recipient)),
                })
                .expect("choose the counter recipient");
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().players[0].hand.len(), hand_after_install);
            assert_eq!(
                runner.state().objects[&recipient]
                    .counters
                    .get(&CounterType::Plus1Plus1),
                Some(&1)
            );
        } else if !late && name == "Printed search watcher" {
            runner.cast(removal).target_object(victim).commit();
            runner
                .act(GameAction::PassPriority)
                .expect("pass to Murder");
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Murder");
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
            assert_eq!(runner.state().stack.len(), 1);
            assert_eq!(runner.state().stack[0].source_id, spell);
            let firing = runner.state().stack[0].id;
            runner
                .act(GameAction::PassPriority)
                .expect("pass to the Rushed Rebirth trigger");
            let resolution = runner
                .act(GameAction::PassPriority)
                .expect("resolve the Rushed Rebirth trigger");
            assert!(resolution.events.iter().any(|event| matches!(
                event, GameEvent::StackResolved { object_id } if *object_id == firing
            )));
            // Existing payload defect: the lesser-MV search uses a cost-object
            // reference. The trigger settles without offering the eligible creature.
            assert_eq!(
                runner.state().objects[&lesser.expect("seed the lesser-MV creature")].zone,
                Zone::Library
            );
        } else {
            let death = runner.cast(removal).target_object(victim).resolve();
            assert_eq!(death.zone_of(victim), Zone::Graveyard);
            match (late, name) {
                (true, _) => {}
                (false, "Printed reanimation watcher") => {
                    // Existing payload defect: the graveyard choice is not restricted to the victim's owner.
                    let WaitingFor::EffectZoneChoice {
                        player: P0, cards, ..
                    } = &runner.state().waiting_for
                    else {
                        panic!(
                            "expected a graveyard choice, got {:?}",
                            runner.state().waiting_for
                        )
                    };
                    assert!(cards.contains(&caster_grave) && cards.contains(&owner_grave));
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![caster_grave],
                        })
                        .expect("select a graveyard card");
                    assert_eq!(
                        runner.state().objects[&caster_grave].zone,
                        Zone::Battlefield
                    );
                    assert_eq!(runner.state().objects[&caster_grave].controller, P0);
                }
                (false, "End-Blaze Epiphany") => {
                    let exiled: Vec<_> = death
                        .events()
                        .iter()
                        .filter_map(|event| match event {
                            GameEvent::ZoneChanged {
                                object_id,
                                from: Some(Zone::Library),
                                to: Zone::Exile,
                                ..
                            } => Some(*object_id),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        exiled.len(),
                        3,
                        "use the watched creature's departure power"
                    );
                    assert!(exiled.contains(&top));
                    let WaitingFor::ChooseFromZoneChoice {
                        player: P0, cards, ..
                    } = &runner.state().waiting_for
                    else {
                        panic!(
                            "expected an exile choice, got {:?}",
                            runner.state().waiting_for
                        )
                    };
                    assert_eq!(cards.len(), exiled.len());
                    assert!(exiled.iter().all(|id| cards.contains(id)));
                    assert!(!cards.contains(&unrelated_exile));
                    runner
                        .act(GameAction::SelectCards { cards: vec![top] })
                        .expect("select an exiled card");
                    for id in exiled.into_iter().chain([unrelated_exile]) {
                        let can_play = runner.state().objects[&id].casting_permissions.iter().any(
                            |permission| {
                                matches!(
                                    permission,
                                    CastingPermission::PlayFromExile { granted_to: P0, .. }
                                )
                            },
                        );
                        assert_eq!(can_play, id == top, "permission applies to the chosen card");
                    }
                }
                _ => unreachable!(),
            }
        }
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn initiate_of_blood_expires_its_watch_on_a_creature_dealt_damage_this_turn() {
    exercise_initiate_of_blood_watch(true);
}

#[test]
fn initiate_of_blood_fires_its_existing_flip_payload_this_turn() {
    exercise_initiate_of_blood_watch(false);
}

fn exercise_initiate_of_blood_watch(late: bool) {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::EffectKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let db =
        engine::database::card_db::CardDatabase::from_integration_fixture_typing_faces_on_read()
            .expect("integration card fixture must load");
    let source = scenario.add_real_card(P0, "Initiate of Blood", Zone::Battlefield, &db);
    let victim = scenario.add_creature(P1, "Damaged creature", 4, 4).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().objects[&source].back_face.is_some());
    let announced = runner.state().objects[&victim].incarnation;
    let damaged = runner.cast(shock).target_object(victim).resolve();
    assert!(damaged.events().iter().any(|event| matches!(
        event,
        GameEvent::DamageDealt { target: TargetRef::Object(id), .. } if *id == victim
    )));
    runner.activate(source, 0).target_object(victim).resolve();
    let [watcher] = runner.state().delayed_triggers.as_slice() else {
        panic!("the damage history makes the selected creature eligible for the watcher");
    };
    let pin = watcher
        .ability
        .context
        .delayed_watch_incarnation
        .expect("announced watch pin");
    assert_eq!((pin.object_id, pin.incarnation), (victim, announced));
    if late {
        advance_past_end_of_turn(&mut runner);
        assert!(runner.state().delayed_triggers.is_empty());
        assert_eq!(runner.state().objects[&victim].incarnation, announced);
    } else {
        runner
            .act(GameAction::PassPriority)
            .expect("give P1 priority");
    }
    let death = runner.cast(removal).target_object(victim).resolve();
    assert_eq!(death.zone_of(victim), Zone::Graveyard);
    assert_eq!(death.events().iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::FlipPermanent, source_id, .. } if *source_id == source)), !late);
    // Existing payload defect: the flip refers to the dead creature, leaving Initiate unflipped.
    assert!(!death.state().objects[&source].flipped);
    assert!(death.state().stack.is_empty());
    assert!(matches!(
        death.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn bounded_death_watchers_queue_in_apnap_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let victim = scenario.add_creature(P0, "Shared target", 2, 2).id();
    let [active_spell, nonactive_spell] = [P0, P1].map(|player| {
        scenario.add_spell_to_hand_from_oracle(
            player, "Printed draw watcher", true,
            "Target creature gets +2/+0 until end of turn. When that creature dies this turn, you draw a card.",
        ).id()
    });
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    runner.cast(active_spell).target_object(victim).resolve();
    runner
        .act(GameAction::PassPriority)
        .expect("pass to the nonactive player");
    runner.cast(nonactive_spell).target_object(victim).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 2);
    for watcher in &runner.state().delayed_triggers {
        assert_eq!(
            watcher
                .ability
                .context
                .delayed_watch_incarnation
                .expect("announced watch pin")
                .object_id,
            victim
        );
    }
    runner.cast(removal).target_object(victim).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("pass to removal");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve removal");
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        [active_spell, nonactive_spell],
    );
    let active_hand = runner.state().players[0].hand.len();
    let nonactive_hand = runner.state().players[1].hand.len();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].hand.len(), active_hand + 1);
    assert_eq!(runner.state().players[1].hand.len(), nonactive_hand + 1);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn burn_away_bounds_the_watcher_with_its_existing_single_object_payload() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));
    stock_libraries(&mut scenario);
    let victim = scenario
        .add_creature(P1, "Announced creature", 7, 7)
        .controlled_by(P0)
        .id();
    let owner_card = scenario
        .add_land_to_graveyard(P1, "Owner's graveyard card")
        .id();
    let controller_card = scenario
        .add_land_to_graveyard(P0, "Controller's graveyard card")
        .id();
    let oracle = "Burn Away deals 6 damage to target creature. When that creature dies this turn, exile its controller's graveyard.";
    let (name, _) = oracle
        .split_once(" deals ")
        .expect("Oracle text names its source");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let announced = runner.state().objects[&victim].incarnation;
    let first = runner.cast(spell).target_object(victim).resolve();
    assert_eq!(first.zone_of(victim), Zone::Battlefield);
    let [watcher] = first.state().delayed_triggers.as_slice() else {
        panic!("the first spell must install the watcher");
    };
    let pin = watcher
        .ability
        .context
        .delayed_watch_incarnation
        .expect("watch pin");
    assert_eq!((pin.object_id, pin.incarnation), (victim, announced));
    assert!(matches!(
        &watcher.condition,
        DelayedTriggerCondition::WheneverEvent {
            expiry: WheneverEventExpiry::EndOfTurn,
            ..
        }
    ));
    let death = runner.cast(removal).target_object(victim).resolve();
    assert!(death.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
            if *object_id == victim
    )));
    // The registry keeps the independent single-object exile defect open.
    assert_eq!(death.zone_of(victim), Zone::Exile);
    assert_eq!(death.zone_of(owner_card), Zone::Graveyard);
    assert_eq!(death.zone_of(controller_card), Zone::Graveyard);
    assert!(death.state().stack.is_empty());
    assert!(matches!(
        death.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn grim_javelineer_surveillance_uses_the_selected_attackers_death() {
    use engine::game::combat::AttackTarget;

    for put_in_graveyard in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::BeginCombat);
        scenario.with_mana_pool(P0, mana(8));
        stock_libraries(&mut scenario);
        let top = scenario.add_card_to_library_top(P0, "Surveil choice");
        scenario.add_creature_from_oracle(
            P0, "Printed attack watcher", 2, 2,
            "Whenever you attack, target attacking creature gets +1/+0 until end of turn. When that creature dies this turn, surveil 1. (Look at the top card of your library. You may put that card into your graveyard.)",
        );
        let attacker = scenario.add_creature(P0, "Selected attacker", 2, 2).id();
        let other = scenario.add_creature(P0, "Other attacker", 2, 2).id();
        let removal = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
            .id();
        let mut runner = scenario.build();
        let announced = runner.state().objects[&attacker].incarnation;
        runner
            .declare_attackers(&[
                (attacker, AttackTarget::Player(P1)),
                (other, AttackTarget::Player(P1)),
            ])
            .expect("declare both attackers");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { player: P0, .. }
        ));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(attacker)),
            })
            .expect("choose attacking creature");
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&attacker].power, Some(3));
        assert_eq!(runner.state().objects[&other].power, Some(2));
        let [watcher] = runner.state().delayed_triggers.as_slice() else {
            panic!("the attack trigger must install the delayed watcher");
        };
        let pin = watcher
            .ability
            .context
            .delayed_watch_incarnation
            .expect("announced attack target");
        assert_eq!((pin.object_id, pin.incarnation), (attacker, announced));
        runner.cast(removal).target_object(attacker).commit();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&attacker].zone, Zone::Graveyard);
        assert!(
            matches!(&runner.state().waiting_for, WaitingFor::SurveilChoice { player: P0, cards, .. } if cards == &[top])
        );
        runner
            .act(GameAction::SelectCards {
                cards: if put_in_graveyard { vec![] } else { vec![top] },
            })
            .expect("answer surveil");
        assert_eq!(
            runner.state().objects[&top].zone,
            if put_in_graveyard {
                Zone::Graveyard
            } else {
                Zone::Library
            }
        );
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn kelsien_advances_experience_for_the_announced_creatures_death() {
    use engine::types::player::PlayerCounterKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));
    stock_libraries(&mut scenario);
    let source = scenario.add_creature_from_oracle(
        P0, "Kelsien", 2, 2,
        "Vigilance, haste\nKelsien gets +1/+1 for each experience counter you have.\n{T}: Kelsien deals 1 damage to target creature you don't control. When that creature dies this turn, you get an experience counter.",
    ).id();
    let victim = scenario.add_creature(P1, "Announced creature", 2, 2).id();
    let bystander = scenario.add_creature(P1, "Unannounced creature", 2, 2).id();
    let second = scenario
        .add_creature(P1, "Second announced creature", 2, 2)
        .id();
    let elixir = scenario.add_artifact_from_oracle(
        P0, "Printed untap artifact",
        "You may activate abilities of creatures you control as though those creatures had haste.\n{1}, {T}: Untap target creature.",
    ).id();
    let destroy_bystander = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let second_removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let announced = runner.state().objects[&victim].incarnation;
    runner.activate(source, 0).target_object(victim).resolve();
    let [watcher] = runner.state().delayed_triggers.as_slice() else {
        panic!("the activation must install a watcher");
    };
    let pin = watcher
        .ability
        .context
        .delayed_watch_incarnation
        .expect("watch pin");
    assert_eq!((pin.object_id, pin.incarnation), (victim, announced));
    runner.activate(elixir, 0).target_object(source).resolve();
    let second_occurrence = runner.state().objects[&second].incarnation;
    runner.activate(source, 0).target_object(second).resolve();
    let pins: Vec<_> = runner
        .state()
        .delayed_triggers
        .iter()
        .map(|watcher| {
            let pin = watcher
                .ability
                .context
                .delayed_watch_incarnation
                .expect("watch pin");
            (pin.object_id, pin.incarnation)
        })
        .collect();
    assert_eq!(pins, [(victim, announced), (second, second_occurrence)]);
    let wrong = runner
        .cast(destroy_bystander)
        .target_object(bystander)
        .resolve();
    assert_eq!(wrong.zone_of(bystander), Zone::Graveyard);
    assert_eq!(
        wrong.state().players[0].player_counter(&PlayerCounterKind::Experience),
        0
    );
    let death = runner.cast(removal).target_object(victim).resolve();
    assert_eq!(death.zone_of(victim), Zone::Graveyard);
    assert_eq!(
        death.state().players[0].player_counter(&PlayerCounterKind::Experience),
        1
    );
    assert!(death.state().stack.is_empty());
    assert!(matches!(
        death.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let second_death = runner.cast(second_removal).target_object(second).resolve();
    assert_eq!(second_death.zone_of(second), Zone::Graveyard);
    assert_eq!(
        second_death.state().players[0].player_counter(&PlayerCounterKind::Experience),
        2
    );
    assert!(second_death.state().stack.is_empty());
    assert!(matches!(
        second_death.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn kelsien_does_not_install_when_its_target_dies_in_response() {
    use engine::types::game_state::CastPaymentMode;
    use engine::types::player::PlayerCounterKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(
        P0, "Kelsien", 2, 2,
        "Vigilance, haste\nKelsien gets +1/+1 for each experience counter you have.\n{T}: Kelsien deals 1 damage to target creature you don't control. When that creature dies this turn, you get an experience counter.",
    ).id();
    let victim = scenario
        .add_creature(P1, "Initially legal target", 2, 2)
        .id();
    scenario.add_creature(P1, "Alternative legal target", 2, 2);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Kelsien's activation");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("target the opposing creature");
    assert_eq!(runner.state().stack.len(), 1);
    let activation = &runner.state().stack[0];
    assert_eq!(activation.source_id, source);
    assert_eq!(
        activation.ability().unwrap().targets,
        [TargetRef::Object(victim)]
    );
    let result = runner.cast(murder).target_object(victim).resolve();
    assert_eq!(result.zone_of(victim), Zone::Graveyard);
    // CR 608.2b: the activation's only target is now illegal.
    assert!(result.state().delayed_triggers.is_empty());
    assert_eq!(
        result.state().players[0].player_counter(&PlayerCounterKind::Experience),
        0
    );
    assert!(result.state().stack.is_empty());
    assert!(matches!(
        result.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn fatal_fissure_targets_a_land_on_the_watched_death_and_preserves_its_return() {
    use engine::types::counter::CounterType;
    use engine::types::mana::ManaColor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));
    stock_libraries(&mut scenario);
    let victim = scenario.add_creature(P1, "Announced creature", 2, 2).id();
    let land = scenario.add_basic_land(P0, ManaColor::Black);
    let bystander = scenario.add_basic_land(P0, ManaColor::Black);
    let opposing_land = scenario.add_basic_land(P1, ManaColor::Black);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Earthbend watcher", true, FATAL_FISSURE)
        .from_oracle_text_with_keywords(&["Earthbend"], FATAL_FISSURE)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let destroy_land = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let spell = runner.cast(spell).target_object(victim).resolve();
    let [watcher] = spell.state().delayed_triggers.as_slice() else {
        panic!("the initial spell must install a watcher");
    };
    let pin = watcher
        .ability
        .context
        .delayed_watch_incarnation
        .expect("watch pin");
    assert_eq!(pin.object_id, victim);
    assert!(matches!(
        &watcher.condition,
        DelayedTriggerCondition::WheneverEvent {
            expiry: WheneverEventExpiry::EndOfTurn,
            ..
        }
    ));
    runner.cast(removal).target_object(victim).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("pass to removal");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve removal");
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    // CR 701.66a: Earthbend targets a land controlled by its controller.
    let WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("P0 must choose the Earthbend target");
    };
    let [slot] = target_slots.as_slice() else {
        panic!("Earthbend has one target slot");
    };
    assert_eq!(slot.legal_targets.len(), 2);
    assert!(slot.legal_targets.contains(&TargetRef::Object(land)));
    assert!(slot.legal_targets.contains(&TargetRef::Object(bystander)));
    assert!(!slot
        .legal_targets
        .contains(&TargetRef::Object(opposing_land)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(land)),
        })
        .expect("choose the Earthbend land");
    runner.advance_until_stack_empty();
    // CR 701.66a: the creature keeps its land type and gains haste and counters.
    let animated_land = &runner.state().objects[&land];
    assert!(animated_land
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Land));
    assert!(animated_land.has_keyword(&engine::types::Keyword::Haste));
    assert_eq!(animated_land.power, Some(4));
    assert_eq!(
        runner.state().objects[&land]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    assert!(runner.state().objects[&bystander].counters.is_empty());
    let animated = runner.state().objects[&land].incarnation;
    let resolved = runner.cast(destroy_land).target_object(land).resolve();
    assert!(resolved.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
            if *object_id == land
    )));
    assert_eq!(resolved.zone_of(land), Zone::Battlefield);
    assert_ne!(resolved.state().objects[&land].incarnation, animated);
    assert!(resolved.is_tapped(land));
    assert!(resolved.state().stack.is_empty());
    assert!(matches!(
        resolved.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn time_to_feed_watches_the_opposing_fighter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));
    stock_libraries(&mut scenario);
    let friendly = scenario.add_creature(P0, "Friendly fighter", 4, 4).id();
    let enemy = scenario.add_creature(P1, "Opposing fighter", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Printed fight watcher", false, TIME_TO_FEED)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_object(enemy)
        .target_object(friendly)
        .resolve();
    assert_eq!(outcome.zone_of(enemy), Zone::Graveyard);
    assert_eq!(outcome.zone_of(friendly), Zone::Battlefield);
    assert_eq!(outcome.life_delta(P0), 3);
    assert!(outcome.state().delayed_triggers.is_empty());
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn time_to_feed_keeps_the_watch_only_when_its_own_target_is_legal() {
    for remove_opponent in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        stock_libraries(&mut scenario);
        let friendly = scenario.add_creature(P0, "Friendly fighter", 4, 4).id();
        let opposing = scenario.add_creature(P1, "Opposing fighter", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Printed fight watcher", false, TIME_TO_FEED)
            .id();
        let response = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
            .id();
        let later = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
            .id();
        let mut runner = scenario.build();
        let life = runner.life(P0);
        runner
            .cast(spell)
            .target_object(opposing)
            .target_object(friendly)
            .commit();
        let removed = if remove_opponent { opposing } else { friendly };
        let survivor = if remove_opponent { friendly } else { opposing };
        runner.cast(response).target_object(removed).resolve();
        assert_eq!(runner.state().objects[&removed].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&survivor].zone, Zone::Battlefield);
        assert!(runner.state().stack.is_empty());
        assert_eq!(runner.life(P0), life);
        if remove_opponent {
            assert!(runner.state().delayed_triggers.is_empty());
        } else {
            let [watcher] = runner.state().delayed_triggers.as_slice() else {
                panic!("the legal opposing target still supports the watch");
            };
            let pin = watcher
                .ability
                .context
                .delayed_watch_incarnation
                .expect("watch pin");
            assert_eq!(
                (pin.object_id, pin.incarnation),
                (opposing, runner.state().objects[&opposing].incarnation)
            );
            let death = runner.cast(later).target_object(opposing).resolve();
            assert_eq!(death.zone_of(opposing), Zone::Graveyard);
            assert_eq!(death.life_delta(P0), 3);
            assert!(death.state().stack.is_empty());
            assert!(matches!(
                death.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
        }
    }
}

#[test]
fn searing_blood_reads_the_selected_creatures_controller_at_death() {
    let previous = engine::types::player::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 291);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));
    stock_libraries(&mut scenario);
    let victim = scenario
        .add_creature(P1, "Borrowed creature", 4, 4)
        .controlled_by(previous)
        .id();
    let oracle = "Searing Blood deals 2 damage to target creature. When that creature dies this turn, Searing Blood deals 3 damage to the creature's controller.";
    let (name, _) = oracle
        .split_once(" deals ")
        .expect("Oracle text names its source");
    let burn = scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .id();
    let treason = scenario.add_spell_to_hand_from_oracle(
        P0, "Act of Treason", false,
        "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.",
    ).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    let announced = runner.state().objects[&victim].incarnation;
    let burn = runner.cast(burn).target_object(victim).resolve();
    assert_eq!(burn.zone_of(victim), Zone::Battlefield);
    assert_eq!(burn.controller(victim), previous);
    let [watcher] = burn.state().delayed_triggers.as_slice() else {
        panic!("surviving the initial damage must leave one watcher");
    };
    let pin = watcher
        .ability
        .context
        .delayed_watch_incarnation
        .expect("watch pin");
    assert_eq!((pin.object_id, pin.incarnation), (victim, announced));
    let stolen = runner.cast(treason).target_object(victim).resolve();
    assert_eq!(stolen.controller(victim), P0);
    assert_eq!(stolen.state().objects[&victim].owner, P1);
    assert_eq!(stolen.state().objects[&victim].incarnation, announced);
    let death = runner.cast(removal).target_object(victim).resolve();
    assert_eq!(death.zone_of(victim), Zone::Graveyard);
    assert_eq!(death.life_delta(P0), -3);
    assert_eq!(death.life_delta(P1), 0);
    assert_eq!(death.life_delta(previous), 0);
    assert!(death.state().stack.is_empty());
    assert!(matches!(
        death.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn printed_pump_watchers_fire_only_for_the_selected_creature_this_turn() {
    use engine::types::mana::ManaColor;

    for (oracle, bonus, token) in [
        ("Target creature gets +2/+0 until end of turn. When that creature dies this turn, you draw a card.", 2, None),
        ("Target creature gets +1/+0 until end of turn. When that creature dies this turn, create a 3/2 red and white Spirit creature token.", 1, Some((&["Spirit"][..], &[ManaColor::Red, ManaColor::White][..]))),
        ("Target creature gets +1/+0 until end of turn. When that creature dies this turn, create a 3/2 colorless Eldrazi Horror creature token.", 1, Some((&["Eldrazi", "Horror"][..], &[][..]))),
    ] {
        for late in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            stock_libraries(&mut scenario);
            scenario.with_mana_pool(P0, mana(8));
            let selected = scenario.add_creature(P0, "Announced creature", 2, 2).id();
            let bystander = scenario.add_creature(P0, "Unannounced creature", 2, 2).id();
            let spell = scenario.add_spell_to_hand_from_oracle(P0, "Printed pump watcher", true, oracle).id();
            let destroy_bystander = scenario
                .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE).id();
            let destroy_selected = scenario
                .add_spell_to_hand_from_oracle(if late { P1 } else { P0 }, "Murder", true, DESTROY_TARGET_CREATURE).id();
            let mut runner = scenario.build();
            let announced = runner.state().objects[&selected].incarnation;
            let cast = runner.cast(spell).target_object(selected).resolve();
            assert_eq!(cast.state().objects[&selected].power, Some(2 + bonus));
            let [watcher] = cast.state().delayed_triggers.as_slice() else {
                panic!("the spell must install one watcher");
            };
            assert!(matches!(
                &watcher.condition,
                DelayedTriggerCondition::WheneverEvent { expiry: WheneverEventExpiry::EndOfTurn, .. }
            ));
            let pin = watcher.ability.context.delayed_watch_incarnation.expect("watch pin");
            assert_eq!((pin.object_id, pin.incarnation), (selected, announced));

            let wrong = runner.cast(destroy_bystander).target_object(bystander).resolve();
            assert_eq!(wrong.zone_of(bystander), Zone::Graveyard);
            assert_eq!(wrong.hand_drawn(P0), 0);
            assert!(!wrong.state().objects.values().any(|object| object.is_token));
            if late {
                advance_past_end_of_turn(&mut runner);
                assert_eq!(runner.state().objects[&selected].incarnation, announced);
                assert!(runner.state().delayed_triggers.is_empty());
            }
            let death = runner.cast(destroy_selected).target_object(selected).resolve();
            assert_eq!(death.zone_of(selected), Zone::Graveyard);
            let created: Vec<_> = death.state().objects.values().filter(|object| object.is_token).collect();
            if let (false, Some((subtypes, colors))) = (late, token) {
                let [created] = created.as_slice() else {
                    panic!("the selected death must create exactly one token");
                };
                // CR 111.3: the effect defines the token's characteristics.
                assert_eq!((created.power, created.toughness), (Some(3), Some(2)));
                assert_eq!(created.zone, Zone::Battlefield);
                assert_eq!(created.card_types.core_types, [engine::types::card_type::CoreType::Creature]);
                assert_eq!(created.card_types.subtypes.len(), subtypes.len());
                assert!(subtypes.iter().all(|expected| created.card_types.subtypes.iter().any(|actual| actual == expected)));
                assert_eq!(created.color.len(), colors.len());
                assert!(colors.iter().all(|color| created.color.contains(color)));
            } else {
                assert!(created.is_empty());
            }
            assert_eq!(death.hand_drawn(P0), i64::from(!late && token.is_none()));
            assert!(death.state().stack.is_empty());
            assert!(matches!(death.final_waiting_for(), WaitingFor::Priority { .. }));
        }
    }
}

#[test]
fn cryptek_returns_the_graveyard_arrival_tapped_under_the_creating_controller() {
    for late in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana(8));
        stock_libraries(&mut scenario);
        let source = scenario
            .add_creature_from_oracle(P0, "Bounded artifact return", 3, 3, CRYPTEK)
            .as_artifact_creature()
            .id();
        let victim = scenario
            .add_creature(P1, "Borrowed artifact creature", 2, 2)
            .as_artifact_creature()
            .controlled_by(P0)
            .id();
        let removal = scenario
            .add_spell_to_hand_from_oracle(
                if late { P1 } else { P0 },
                "Murder",
                true,
                DESTROY_TARGET_CREATURE,
            )
            .id();
        let steal = (!late).then(|| scenario.add_spell_to_hand_from_oracle(
            P1, "Act of Aggression",
            true,
            "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.",
        ).id());
        let mut runner = scenario.build();
        runner.activate(source, 0).target_object(victim).resolve();
        let [watcher] = runner.state().delayed_triggers.as_slice() else {
            panic!("the activation must create exactly one watcher");
        };
        let pin = watcher
            .ability
            .context
            .delayed_watch_incarnation
            .expect("watch pin");
        assert_eq!(
            (pin.object_id, pin.incarnation),
            (victim, runner.state().objects[&victim].incarnation)
        );
        assert!(matches!(
            &watcher.condition,
            DelayedTriggerCondition::WheneverEvent {
                expiry: WheneverEventExpiry::EndOfTurn,
                ..
            }
        ));
        if late {
            advance_past_end_of_turn(&mut runner);
            assert!(runner.state().delayed_triggers.is_empty());
            assert_eq!(runner.state().objects[&victim].incarnation, pin.incarnation);
        }
        if let Some(steal) = steal {
            runner
                .act(GameAction::PassPriority)
                .expect("give P1 priority");
            let stolen = runner.cast(steal).target_object(source).resolve();
            assert_eq!(stolen.controller(source), P1);
        }
        let result = runner.cast(removal).target_object(victim).resolve();
        assert!(result.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
            if *object_id == victim
    )));
        assert_eq!(result.state().objects[&victim].owner, P1);
        if late {
            assert_eq!(result.zone_of(victim), Zone::Graveyard);
        } else {
            // CR 603.7e: the delayed trigger keeps the creating ability's controller.
            assert_eq!(result.controller(source), P1);
            assert_eq!(result.zone_of(victim), Zone::Battlefield);
            assert_eq!(result.controller(victim), P0);
            assert!(result.is_tapped(victim));
        }
        assert!(result.state().stack.is_empty());
        assert!(matches!(
            result.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn cryptek_cannot_target_itself() {
    use engine::types::game_state::CastPaymentMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(2));
    let source = scenario
        .add_creature_from_oracle(P0, "Bounded artifact return", 3, 3, CRYPTEK)
        .as_artifact_creature()
        .id();
    let other = scenario
        .add_creature(P0, "Another artifact creature", 2, 2)
        .as_artifact_creature()
        .id();
    let alternative = scenario
        .add_creature(P0, "Alternative artifact creature", 2, 2)
        .as_artifact_creature()
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Cryptek's activation");
    let WaitingFor::TargetSelection {
        player: P0,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Cryptek must ask P0 for a target");
    };
    // CR 115.1c: the printed target restriction excludes Cryptek.
    assert_eq!(target_slots[0].legal_targets.len(), 2);
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(other)));
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(alternative)));
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(source))
        })
        .is_err());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(other)),
        })
        .expect("the other artifact creature is legal");
    runner.advance_until_stack_empty();
    let [watcher] = runner.state().delayed_triggers.as_slice() else {
        panic!("the legal selection must install a watcher");
    };
    assert_eq!(
        watcher
            .ability
            .context
            .delayed_watch_incarnation
            .unwrap()
            .object_id,
        other
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// Flicker supplies the immediate exile and return. A Good Day to Die supplies
// the independent creature declaration and death watcher. The fixture needs
// both instructions in one resolution so the two announced pins predate the
// blink. The first declaration names Artifact so only slot 1 names Creature.
const BLINK_AND_WATCH: &str = "Exile target artifact. Return that card to the battlefield under its owner's control. Target creature gets +2/+0 until end of turn. When that creature dies this turn, you draw a card.";

#[test]
fn declared_death_watcher_checks_its_positional_pin_after_a_blink() {
    for same_object in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana(8));
        stock_libraries(&mut scenario);
        let artifact = scenario
            .add_creature(P0, "Blink subject", 2, 2)
            .as_artifact_creature()
            .id();
        let creature = scenario.add_creature(P0, "Independent creature", 2, 2).id();
        let selected = if same_object { artifact } else { creature };
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Structural blink and watch", true, BLINK_AND_WATCH)
            .id();
        let removal = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
            .id();
        let mut runner = scenario.build();
        let original = runner.state().objects[&artifact].incarnation;
        let selected_pin = runner.state().objects[&selected].incarnation;
        let cast = runner
            .cast(spell)
            .target_object(artifact)
            .target_object(selected)
            .commit();
        let root = cast
            .state()
            .stack
            .last()
            .and_then(|entry| entry.ability())
            .expect("announced spell");
        let contextual_return = root.sub_ability.as_deref().expect("contextual return");
        let pump = contextual_return
            .sub_ability
            .as_deref()
            .expect("independent pump");
        let installer = pump.sub_ability.as_deref().expect("death watcher");
        assert!(matches!(
            root.effect,
            Effect::ChangeZone { target: TargetFilter::Typed(ref filter), .. }
                if filter.type_filters == vec![TypeFilter::Artifact]
        ));
        assert!(matches!(
            contextual_return.effect,
            Effect::ChangeZone {
                target: TargetFilter::ParentTarget,
                ..
            }
        ));
        assert!(matches!(
            pump.effect,
            Effect::Pump { target: TargetFilter::Typed(ref filter), .. }
                if filter.type_filters == vec![TypeFilter::Creature]
        ));
        assert_eq!(root.targets, [TargetRef::Object(artifact)]);
        assert!(contextual_return.target_pins.is_empty());
        assert_eq!(pump.targets, [TargetRef::Object(selected)]);
        assert_eq!(
            pump.target_pins[0].expect("slot 1 pin").incarnation,
            selected_pin
        );
        assert!(matches!(
            &installer.effect,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WheneverEvent { trigger, .. },
                ..
            } if trigger.valid_card == Some(TargetFilter::ParentTargetSlot { index: 1 })
        ));
        let installer_before_blink = (*installer).clone();
        let slot_one = pump.target_pins[0].expect("slot 1 pin");
        let outcome = cast.resolve();
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Exile, .. }
                if *object_id == artifact
        )));
        assert_eq!(outcome.zone_of(artifact), Zone::Battlefield);
        assert_ne!(outcome.state().objects[&artifact].incarnation, original);
        if same_object {
            // The shared slot lookup checks this ID projection after finding
            // the slot. The inherited installer passes it; slot 1 still
            // names the first battlefield occurrence.
            assert!(installer_before_blink.target_pin_is_current(selected, outcome.state()));
            assert_ne!(
                slot_one.incarnation,
                outcome.state().objects[&selected].incarnation
            );
            assert!(outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::CreateDelayedTrigger, source_id, .. }
                    if *source_id == spell
            )));
            assert!(outcome.state().delayed_triggers.is_empty());
        } else {
            let [watcher] = outcome.state().delayed_triggers.as_slice() else {
                panic!("the independently selected creature must be watched");
            };
            let pin = watcher
                .ability
                .context
                .delayed_watch_incarnation
                .expect("watch pin");
            assert_eq!((pin.object_id, pin.incarnation), (selected, selected_pin));
        }
        let killed = runner.cast(removal).target_object(selected).resolve();
        assert_eq!(killed.zone_of(selected), Zone::Graveyard);
        assert_eq!(killed.hand_drawn(P0), if same_object { 0 } else { 1 });
        assert!(killed.state().stack.is_empty());
        assert!(matches!(
            killed.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn together_forever_returns_the_matched_arrival_and_expires_at_cleanup() {
    for (late, move_arrival, blinked) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (false, false, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, [mana(8), mana_of(ManaType::White, 2)].concat());
        for _ in 0..3 {
            scenario.add_basic_land(P1, engine::types::mana::ManaColor::Black);
        }
        stock_libraries(&mut scenario);
        let source = scenario
            .add_enchantment_from_oracle(P0, "Bounded return", TOGETHER_FOREVER)
            .id();
        let victim = scenario.add_creature(P1, "Opponent's creature", 2, 2).id();
        scenario.with_counter(victim, engine::types::counter::CounterType::Plus1Plus1, 1);
        let hexmage = scenario
            .add_creature_from_oracle(
                P0,
                "Vampire Hexmage",
                2,
                1,
                "First strike\nSacrifice this creature: Remove all counters from target permanent.",
            )
            .id();
        let blink = scenario.add_spell_to_hand_from_oracle(
            P0, "Flicker", false, "Exile target nontoken permanent, then return it to the battlefield under its owner's control.",
        ).id();
        let removal = scenario
            .add_spell_to_hand_from_oracle(
                if late { P1 } else { P0 },
                "Murder",
                true,
                DESTROY_TARGET_CREATURE,
            )
            .id();
        let cremate = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Cremate",
                true,
                "Exile target card from a graveyard.\nDraw a card.",
            )
            .id();
        let pull = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Pull from Eternity",
                true,
                "Put target face-up exiled card into its owner's graveyard.",
            )
            .id();
        let mut runner = scenario.build();
        let announced = runner.state().objects[&victim].incarnation;
        runner.activate(source, 0).target_object(victim).resolve();
        let [watcher] = runner.state().delayed_triggers.as_slice() else {
            panic!("the activation must install exactly one watcher");
        };
        assert!(matches!(
            &watcher.condition,
            DelayedTriggerCondition::WheneverEvent { trigger, expiry: WheneverEventExpiry::EndOfTurn }
                if trigger.mode == TriggerMode::ChangesZone
                    && trigger.origin == Some(Zone::Battlefield)
                    && trigger.destination == Some(Zone::Graveyard)
                    && trigger.valid_card == Some(TargetFilter::SpecificObject { id: victim })
        ));
        let watched = watcher
            .ability
            .context
            .delayed_watch_incarnation
            .expect("announced watch pin");
        assert_eq!(
            (watched.object_id, watched.incarnation),
            (victim, announced)
        );
        let saved = serde_json::to_vec(runner.state()).expect("save installed watcher");
        *runner.state_mut() = serde_json::from_slice(&saved).expect("restore installed watcher");
        assert_eq!(
            runner.state().delayed_triggers[0]
                .ability
                .context
                .delayed_watch_incarnation,
            Some(watched)
        );
        runner.activate(hexmage, 0).target_object(victim).resolve();
        assert!(runner.state().objects[&victim].counters.is_empty());
        if blinked {
            runner.cast(blink).target_object(victim).resolve();
            assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
            assert_ne!(runner.state().objects[&victim].incarnation, announced);
            assert_eq!(runner.state().delayed_triggers.len(), 1);
        }
        if late {
            advance_past_end_of_turn(&mut runner);
            assert!(runner.state().delayed_triggers.is_empty());
            assert_eq!(runner.state().objects[&victim].incarnation, announced);
        }
        let probe_watcher = (!late && !blinked && !move_arrival)
            .then(|| runner.state().delayed_triggers[0].clone());
        runner.cast(removal).target_object(victim).commit();
        runner
            .act(GameAction::PassPriority)
            .expect("pass to resolve removal");
        let death = runner
            .act(GameAction::PassPriority)
            .expect("resolve removal");
        let event = death.events.iter().find(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
                if *object_id == victim
        )).expect("the selected creature actually died");
        let GameEvent::ZoneChanged { record, .. } = event else {
            unreachable!("the event lookup required a zone change");
        };
        assert_eq!(
            record
                .trigger_source_context()
                .expect("departure context")
                .identity
                .reference
                == watched,
            !blinked
        );
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        let arrival = runner.state().objects[&victim].incarnation;
        assert_ne!(arrival, announced);
        if late || blinked {
            assert!(runner.state().stack.is_empty());
        } else {
            assert_eq!(runner.state().stack.len(), 1);
            assert!(runner.state().delayed_triggers.is_empty());
            if let Some(watcher) = probe_watcher {
                let mut probe = runner.state().clone();
                probe.stack.clear();
                probe.delayed_triggers.push(watcher);
                let mut malformed = event.clone();
                let GameEvent::ZoneChanged { record, .. } = &mut malformed else {
                    unreachable!("cloned a zone change");
                };
                record.from_zone = Some(Zone::Exile);
                assert_eq!(
                    record
                        .trigger_source_context()
                        .expect("raw context")
                        .identity
                        .reference,
                    watched
                );
                engine::game::triggers::check_delayed_triggers(&mut probe, &[malformed]);
                assert!(
                    probe.stack.is_empty(),
                    "a matching raw pin cannot authorize a contradictory record"
                );
                engine::game::triggers::check_delayed_triggers(
                    &mut probe,
                    &[event.clone(), event.clone()],
                );
                assert_eq!(
                    probe.stack.len(),
                    1,
                    "duplicate records of the same coherent departure produce one firing"
                );
            }
            let queued = runner.state().stack[0].ability().expect("queued return");
            assert_eq!(queued.context.delayed_watch_incarnation, Some(watched));
            let arrival_pin = engine::types::identifiers::ObjectIncarnationRef::of(victim, arrival);
            assert_eq!(queued.target_incarnations, [arrival_pin]);
            let saved = serde_json::to_vec(runner.state()).expect("save queued return");
            *runner.state_mut() = serde_json::from_slice(&saved).expect("restore queued return");
            let restored = runner.state().stack[0].ability().expect("restored return");
            assert_eq!(restored.context.delayed_watch_incarnation, Some(watched));
            assert_eq!(restored.target_incarnations, [arrival_pin]);
            if move_arrival {
                runner.cast(cremate).target_object(victim).commit();
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass to Cremate");
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Cremate");
                assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
                runner.cast(pull).target_object(victim).commit();
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass to Pull from Eternity");
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Pull from Eternity");
                assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
                assert_ne!(runner.state().objects[&victim].incarnation, arrival);
                assert_eq!(runner.state().stack.len(), 1);
            }
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().objects[&victim].zone,
                if move_arrival {
                    Zone::Graveyard
                } else {
                    Zone::Hand
                }
            );
            assert_eq!(
                runner.state().players[P1.0 as usize].hand.contains(&victim),
                !move_arrival
            );
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

// -------------------------------------------------------------------- helpers

fn mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

/// Coloured mana for cards whose costs are not generic-payable from `mana()`'s
/// black pool — Whippoorwill's `{G}{G}` activation and Ephemerate's `{W}`.
fn mana_of(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

/// Give both players a library so crossing a turn boundary does not end the
/// game by decking (CR 704.5b).
///
/// Required by any test whose delayed trigger resolves on a LATER turn — T-Z5's
/// `AtNextPhaseForPlayer { Upkeep }` crosses two draw steps. Without it the
/// advance loop dies with `GameOver`, which would be a harness artifact
/// masquerading as a result.
fn stock_libraries(scenario: &mut GameScenario) {
    for i in 0..40 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("Filler {i}"));
    }
}

/// Advance until every delayed trigger has fired and the stack is empty,
/// returning every event the engine emitted along the way.
///
/// Adapted from `issue_2424_goryos_vengeance.rs:122-154` (declare-attackers /
/// declare-blockers / pass-priority with a 256-iteration guard). Kept local so
/// the existing Goryo's file is not modified.
fn advance_until_delayed_triggers_resolve(
    runner: &mut engine::game::scenario::GameRunner,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "delayed trigger never resolved; phase = {:?}, waiting_for = {:?}, dt = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            // CR 514.1: a delayed trigger that resolves on a LATER turn (T-Z5's
            // `AtNextPhaseForPlayer { Upkeep }`) makes the loop cross a cleanup
            // step, where a stocked library has pushed the player over seven
            // cards. Discarding down is required to keep advancing; a
            // `PassPriority` here is rejected outright.
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            _ => GameAction::PassPriority,
        };
        match runner.act(action) {
            Ok(result) => events.extend(result.events),
            Err(e) => panic!(
                "advancing to the delayed trigger failed: {e:?} (waiting_for = {:?})",
                runner.state().waiting_for
            ),
        }
    }
    events
}

/// Advance to the end of the current turn, regardless of whether any delayed
/// trigger fires.
///
/// `advance_until_delayed_triggers_resolve` drains the delayed-trigger list and
/// is the right tool when a trigger is EXPECTED to fire. It is the wrong tool
/// for a negative arm whose trigger legitimately never fires — a "this turn"
/// `WhenDies` trigger on a creature that does not die stays installed until
/// cleanup, so draining would spin until the iteration guard trips and report a
/// harness stall as a result.
fn advance_past_end_of_turn(runner: &mut engine::game::scenario::GameRunner) {
    let start_turn = runner.state().turn_number;
    let mut guard = 0;
    while runner.state().turn_number == start_turn {
        guard += 1;
        assert!(
            guard < 256,
            "turn never ended; phase = {:?}, waiting_for = {:?}",
            runner.state().phase,
            runner.state().waiting_for,
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            _ => GameAction::PassPriority,
        };
        // Never `return` on an error. A silent early exit means no game time
        // passes, and every "the referent survived to end of turn" assertion
        // downstream then holds trivially — the negative arm would pass for the
        // wrong reason. Panic instead, matching
        // `advance_until_delayed_triggers_resolve` above.
        if let Err(e) = runner.act(action) {
            panic!(
                "advancing past end of turn failed: {e:?} (phase = {:?}, waiting_for = {:?})",
                runner.state().phase,
                runner.state().waiting_for,
            );
        }
    }
    assert!(
        runner.state().turn_number > start_turn,
        "reach-guard: the turn must actually have ended for a survival assertion to mean anything"
    );
}

/// True when an `EffectResolved` event names this source — the observable proof
/// that a trigger DID fire and DID resolve (CR 603.7b), even when it affected
/// nothing.
fn effect_resolved_from(events: &[GameEvent], source: ObjectId) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved { source_id, .. } if *source_id == source
        )
    })
}

// ============================================================ T-a (control)

/// T-a — ANTI-VACUITY CONTROL. With no blink, Goryo's delayed trigger really
/// does exile the reanimated creature at the next end step.
///
/// This must fail if the "fix" merely disables the delayed trigger, which is
/// why it is a control rather than a nice-to-have.
#[test]
fn t_a_goryos_exiles_reanimated_creature_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(goryos).target_object(legendary).resolve();
    // Positive reach-guard: the reanimation actually happened, so the end-step
    // assertion below is about the delayed trigger and not about a fizzled cast.
    assert_eq!(
        outcome.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Goryo's must return the creature to the battlefield"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Goryo's must install exactly one delayed exile trigger"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        runner.state().objects[&legendary].zone,
        Zone::Exile,
        "T-a: with no blink the delayed trigger must exile the creature"
    );
}

// ============================================================ T-b (the bug)

/// T-b — THE BUG. Blinking the reanimated creature with Ephemerate makes it a
/// new object (CR 400.7); the delayed trigger must no longer affect it.
///
/// Two independent halves:
///   1. the creature ends on the battlefield (the CR 603.7c note), and
///   2. the trigger STILL fired and resolved as a no-op (CR 603.7b) — evidenced
///      by an `EffectResolved` naming Goryo's.
///
/// Half 2 is the direct test of the early-return event rule: it goes red if the
/// guard returns without pushing `EffectResolved`, while half 1 stays green.
#[test]
fn t_b_blinked_referent_is_not_exiled_but_trigger_still_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .id();

    let mut runner = scenario.build();

    let reanimated = runner.cast(goryos).target_object(legendary).resolve();
    assert_eq!(
        reanimated.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Goryo's must return the creature before it can be blinked"
    );
    let goryos_source = goryos;

    let blinked = runner.cast(ephemerate).target_object(legendary).resolve();
    // Reach-guard: the blink really happened. Without this, "still on the
    // battlefield" at the end step could pass on a creature that never moved.
    assert_eq!(
        blinked.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Ephemerate must return the creature to the battlefield"
    );
    assert!(
        blinked.events().iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if *object_id == legendary
        )),
        "reach-guard: Ephemerate must actually exile the creature (blink leg 1)"
    );

    let events = advance_until_delayed_triggers_resolve(&mut runner);

    // Half 1 — CR 603.7c: it left the zone and returned, so it is a new object.
    assert_eq!(
        runner.state().objects[&legendary].zone,
        Zone::Battlefield,
        "T-b: a blinked referent is a NEW object and must not be exiled by the \
         delayed trigger (CR 400.7 / CR 603.7c)"
    );

    // Half 2 — CR 603.7b: the trigger still fired and still resolved.
    assert!(
        effect_resolved_from(&events, goryos_source),
        "T-b: the delayed trigger must still fire and resolve as a no-op, \
         emitting EffectResolved (CR 603.7b)"
    );
}

// ============================================================ T-c (ruling)

/// T-c — the official ruling's literal case: "If the returned creature leaves
/// the battlefield before the end step, it will remain in its current zone. It
/// won't be exiled."
///
/// The referent left and did NOT return, so it must stay in the graveyard —
/// explicitly distinguished from `Zone::Exile`.
#[test]
fn t_c_referent_that_left_and_did_not_return_stays_put() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    let reanimated = runner.cast(goryos).target_object(legendary).resolve();
    assert_eq!(
        reanimated.zone_of(legendary),
        Zone::Battlefield,
        "reach-guard: Goryo's must return the creature first"
    );

    let killed = runner.cast(removal).target_object(legendary).resolve();
    assert_eq!(
        killed.zone_of(legendary),
        Zone::Graveyard,
        "reach-guard: the removal spell must put the creature in the graveyard"
    );

    let events = advance_until_delayed_triggers_resolve(&mut runner);

    let final_zone = runner.state().objects[&legendary].zone;
    // The two zones are asserted SEPARATELY and explicitly. This was a live
    // pre-fix red (the engine exiled the creature out of the graveyard,
    // contradicting the official ruling), so the `Exile` case is named rather
    // than merely implied: a mis-written single assertion could otherwise pass
    // against the exact zone this test exists to rule out.
    assert_ne!(
        final_zone,
        Zone::Exile,
        "T-c: the delayed trigger must NOT exile a referent that left the \
         battlefield and did not return (CR 603.7c; official ruling: \"If the \
         returned creature leaves the battlefield before the end step, it will \
         remain in its current zone. It won't be exiled.\")"
    );
    assert_eq!(
        final_zone,
        Zone::Graveyard,
        "T-c: the referent left the battlefield and did not return, so it stays \
         in its current zone"
    );
    // Positive reach-guard so the negatives above cannot pass vacuously on a
    // trigger that never fired at all.
    assert!(
        effect_resolved_from(&events, goryos),
        "T-c reach-guard: the delayed trigger must still have fired and resolved"
    );
}

// ====================================================== T-Z1 (MUST STAY GREEN)

/// T-Z1 — MUST-STAY-GREEN CONTROL (Saffi Eriksdotter).
///
/// "Sacrifice Saffi Eriksdotter: When target creature is put into your graveyard
/// this turn, return that card to the battlefield."
///
/// Saffi's delayed trigger's own CONDITION is the referent's zone change, so the
/// referent is EXPECTED to have moved (CR 603.7c operative test; CR 400.7e
/// affirmatively grants that such an ability can find the object in the zone it
/// moved to). Pinning it would make the card a permanent no-op.
///
/// This must pass BOTH before and after the fix. A control that was never green
/// pre-fix proves nothing about regression.
#[test]
fn t_z1_saffi_eriksdotter_still_returns_the_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));

    let saffi = scenario
        .add_creature_from_oracle(P0, "Saffi Eriksdotter", 2, 2, SAFFI_ERIKSDOTTER)
        .id();
    let victim = scenario.add_creature(P0, "Doomed Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    let activation = runner.activate(saffi, 0).target_object(victim).resolve();
    assert_eq!(
        activation.zone_of(saffi),
        Zone::Graveyard,
        "reach-guard: activating Saffi sacrifices her"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Saffi's activation must install exactly one delayed trigger"
    );

    let killed = runner.cast(removal).target_object(victim).resolve();
    // Reach-guard on the EVENT, not on the final zone. A zone snapshot cannot
    // serve here: Saffi's trigger fires on the death and returns the card
    // within this same resolution, so the victim legitimately ends on the
    // battlefield — which is indistinguishable from "the removal never resolved
    // and it never left". Asserting `Battlefield` as both the guard and the
    // conclusion is what made this control vacuous. Prove the death happened
    // instead.
    assert!(
        killed.events().iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Graveyard,
                ..
            } if *object_id == victim
        )),
        "reach-guard: the victim must actually have been put into the graveyard \
         for Saffi's delayed trigger to have anything to return"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "T-Z1: Saffi's delayed trigger names the referent's OWN zone change, so \
         the referent is expected to have moved and must still be affected \
         (CR 603.7c operative test + CR 400.7e)"
    );
}

// ====================================================== T-Z4 (controller restriction)

/// T-Z4 — Lagrella's selected card enters under its owner's control (CR 610.3c).
/// Its delayed "under your control" trigger can affect that card only when
/// its controller is the creating ability's controller (CR 109.5 + CR 603.7e).
///
/// The Lagrella pair distinguishes the `WhenEntersBattlefield` arm: the referent is expected
/// to have moved ONTO the battlefield, and `zones.rs:816` bumps the incarnation
/// unconditionally on `to == Battlefield`, so pinning this condition would make
/// the card a permanent no-op at 100% of firings.
///
/// It also exercises the `counters.rs` direct read and the tracked-set condition
/// erasure at the same time: Lagrella's CDT is `uses_tracked_set: true`, so its
/// condition is rewritten at `bind_tracked_set_to_condition` — which is exactly
/// why the expected-zone gate must read the PARSER-EMITTED condition.
///
#[test]
fn t_z4_lagrella_does_not_counter_opponents_returned_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));

    let lagrella = scenario
        .add_creature_to_hand_from_oracle(P0, "Lagrella, the Magpie", 3, 3, LAGRELLA)
        .as_legendary()
        .id();
    let opposing = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    let entered = runner.cast(lagrella).target_object(opposing).resolve();
    // Reach-guard 1: the exile leg actually happened.
    assert_eq!(
        entered.zone_of(opposing),
        Zone::Exile,
        "reach-guard: Lagrella's ETB must exile the opposing creature"
    );
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    let delayed = &runner.state().delayed_triggers[0];
    assert_eq!(delayed.controller, P0);
    let DelayedTriggerCondition::WhenEntersBattlefield { filter } = &delayed.condition else {
        panic!(
            "reach-guard: expected a delayed entry condition, got {:?}",
            delayed.condition
        );
    };
    let tracked_set_id = match filter {
        TargetFilter::TrackedSet { id } => *id,
        TargetFilter::And { filters } => match filters.first() {
            Some(TargetFilter::TrackedSet { id }) => *id,
            other => panic!("reach-guard: expected a tracked subject, got {other:?}"),
        },
        other => panic!("reach-guard: expected a tracked subject, got {other:?}"),
    };
    assert!(
        runner.state().tracked_object_sets[&tracked_set_id].contains(&opposing),
        "reach-guard: the delayed trigger must use the set produced by Lagrella's exile"
    );

    let removed = runner.cast(removal).target_object(lagrella).resolve();
    assert_eq!(
        removed.zone_of(lagrella),
        Zone::Graveyard,
        "reach-guard: Lagrella must leave the battlefield to return the card"
    );

    // The public cast driver settles the return and stack. A correctly filtered
    // one-shot delayed trigger remains installed because it did not fire.
    assert_eq!(
        runner.state().objects[&opposing].zone,
        Zone::Battlefield,
        "reach-guard: the exiled card must return to the battlefield"
    );
    assert_eq!(runner.state().objects[&opposing].controller, P1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());

    let counters = runner.state().objects[&opposing]
        .counters
        .get(&engine::types::counter::CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 0,
        "T-Z4: a selected card returning under P1's control does not satisfy \
         Lagrella's printed 'under your control' restriction"
    );
}

/// The matching P0 return proves that the controller restriction does not
/// suppress the tracked delayed entry trigger for an eligible card.
#[test]
fn t_z4_lagrella_counters_own_returned_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(12));
    let lagrella = scenario
        .add_creature_to_hand_from_oracle(P0, "Lagrella, the Magpie", 3, 3, LAGRELLA)
        .as_legendary()
        .id();
    let own = scenario.add_creature(P0, "Own Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();

    let entered = runner.cast(lagrella).target_object(own).resolve();
    assert_eq!(entered.zone_of(own), Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    let removed = runner.cast(removal).target_object(lagrella).resolve();
    assert_eq!(removed.zone_of(lagrella), Zone::Graveyard);
    assert_eq!(runner.state().objects[&own].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&own].controller, P0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&own]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "an eligible tracked card returning under P0's control gets two counters"
    );
}

// ==================================================== T-H2 (inertness control)

/// T-H2 — SIBLING / NEGATIVE. A `ParentTarget` reference in a NON-delayed chain
/// resolves unchanged, proving the guard is inert outside the delayed path.
///
/// Goryo's own "That creature gains haste" is a `GenericEffect{ParentTarget}`
/// resolved immediately in the same chain, so it exercises the no-pin arm of the
/// keyed predicate (`find(..).is_none_or(..)`).
#[test]
fn t_h2_non_delayed_parent_target_reference_is_unaffected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(8));

    let legendary = scenario
        .add_creature_to_graveyard(P0, "Legendary Bear", 4, 4)
        .as_legendary()
        .id();
    let goryos = scenario
        .add_spell_to_hand_from_oracle(P0, "Goryo's Vengeance", true, GORYOS_VENGEANCE)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(goryos).target_object(legendary).resolve();

    assert_eq!(
        outcome.zone_of(legendary),
        Zone::Battlefield,
        "T-H2: the immediate ChangeZone link resolves unchanged"
    );
    // The haste grant is the immediate `GenericEffect{ParentTarget}` link. It
    // must still find its referent — no pin exists for a non-delayed chain.
    assert!(
        outcome
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| {
                matches!(
                    tce.affected,
                    engine::types::ability::TargetFilter::SpecificObject { id } if id == legendary
                )
            }),
        "T-H2: the immediate ParentTarget haste grant must still reach its \
         referent — the pin must be inert outside the delayed path"
    );
}

// ================================================ T-D1 (PLACEMENT DETECTOR)

/// T-D1 — **THE §5.3(b) PLACEMENT DETECTOR.** Role: PLACEMENT DETECTOR, and it
/// is the ONLY test in this file that can detect a §5.3(b) misplacement.
///
/// # Why this test exists
///
/// §5.3(b)'s gate must be called BEFORE the two condition binders, because they
/// rewrite `WhenDies { filter: ParentTarget }` into `WhenDies { filter:
/// SpecificObject { id } }` and the gate can no longer recognize the anaphor.
/// That constraint was **unfalsifiable** until this test existed: implementation
/// round 1 sank the call past both binders — reproducing the exact defect the
/// seam exists to prevent — and every then-existing test still passed.
///
/// A detector for this seam needs BOTH properties, and the two tests previously
/// named here satisfy at most one each:
///
/// - **(P1) a pin is actually STAMPED** — requires `parent_target_snapshot` to
///   return a NON-EMPTY list. `saffi eriksdotter` and `adarkar valkyrie` FAIL
///   this: despite Oracle text reading "target creature", neither parse declares
///   a target slot at all, so the snapshot is `[]` and the seam is inert in BOTH
///   placements. **Verify by parse, never by Oracle prose.**
/// - **(P2) the pin is actually READ** — requires the delayed effect to reach a
///   guarded terminal read. `lagrella, the magpie` FAILS this: a pin IS stamped,
///   but her `PutCounter` returns at `counters.rs`'s ungated event-context arm
///   before the guarded read, so the counters land either way.
///
/// `whippoorwill` is the only in-class card clean on both, verified at the card
/// data rather than assumed:
/// - **P1** — its root ability is `kind: Activated`, `effect: GenericEffect` with
///   a real `target: Typed { type_filters: [Creature] }` slot, and the delayed
///   trigger hangs off it via a `SequentialSibling` sub-ability chain
///   (`AddRestriction` → `CreateDelayedTrigger`). `parent_target_snapshot`
///   therefore returns through `parent_chain_targets_from_root` — the branch
///   Saffi and Adarkar Valkyrie never reach.
/// - **P2** — the delayed effect is `ChangeZone { destination: Exile, target:
///   ParentTarget }`, which routes through `resolved_targets` to the guarded
///   terminal read.
/// - `uses_tracked_set: false`, so `bind_contextual_filter_to_condition` is the
///   ONLY rewrite and a red here is single-cause.
///
/// # How it goes red
///
/// With the gate misplaced, `condition_expects_referent_move` is `false`, so a
/// pin IS stamped while the creature is on the battlefield. The creature then
/// dies, which bumps its incarnation, so the pin is stale when the trigger
/// resolves — the guard drops the referent and NOTHING is exiled. The card stays
/// in the graveyard and the `Zone::Exile` assertion fails.
///
/// With the gate correctly placed, no pin is stamped (the condition names the
/// referent's own zone change), the snapshot resolves unfiltered, and the
/// graveyard card is exiled. That is also the shipped behavior today, so this
/// test is green both pre-fix and post-fix — **the red is its job, not the
/// green.**
#[test]
fn t_d1_whippoorwill_exiles_the_dead_referent_from_the_graveyard() {
    // Verbatim Oracle text (Scryfall). A paraphrase can take a different parser
    // branch and go green while the real card stays broken.
    const WHIPPOORWILL: &str = "{G}{G}, {T}: Target creature can't be regenerated this turn. Damage that would be dealt to that creature this turn can't be prevented or dealt instead to another permanent or player. When the creature dies this turn, exile the creature.";

    // ---- Arm 1: the referent dies. The delayed trigger must exile it. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::Green, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let bird = scenario
        .add_creature_from_oracle(P0, "Whippoorwill", 1, 1, WHIPPOORWILL)
        .id();
    let victim = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();

    runner.activate(bird, 0).target_object(victim).resolve();

    // Reach-guard (P1): the activation really did install the delayed trigger.
    // Without this, a parse that silently dropped the CDT would make the whole
    // test vacuous.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: activating Whippoorwill must install exactly one delayed trigger"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "reach-guard: the referent is on the battlefield when the trigger is created"
    );

    let killed = runner.cast(removal).target_object(victim).resolve();

    // Paired positive reach-guard: the referent actually DIED. Without it,
    // "ends in Exile" could pass on a creature exiled by something else, and
    // "not in Graveyard" could pass vacuously on a creature that never died.
    // Both `Graveyard` and `Exile` are accepted here because the delayed trigger
    // may already have resolved inside `resolve()`; the one zone that would
    // falsify the premise is `Battlefield`.
    assert_ne!(
        killed.zone_of(victim),
        Zone::Battlefield,
        "reach-guard: the referent must have died for this test to mean anything"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    let final_zone = runner.state().objects[&victim].zone;
    // The two zones are asserted DISTINCTLY: this is a card-moved-out-of-the-
    // graveyard test, so `Graveyard` is the failure state, not a neutral one.
    assert_ne!(
        final_zone,
        Zone::Graveyard,
        "T-D1: the delayed WhenDies trigger names the referent's OWN zone change, \
         so no pin may be stamped and the dead creature must still be exiled \
         (CR 603.7c operative test + CR 400.7e). Still in the graveyard means a \
         pin WAS stamped — the §5.3(b) gate ran after the condition binders saw \
         the anaphor rewritten"
    );
    assert_eq!(
        final_zone,
        Zone::Exile,
        "T-D1: the referent must end in exile, not merely somewhere other than \
         the graveyard"
    );

    // ---- Arm 2: no kill. The P1 reach-guard for the pair. ----
    // A `[]` snapshot could never have produced arm 1's move, so the two arms
    // together prove a referent was genuinely captured — the property `saffi`
    // silently lacks and the reason round 1's revert-check produced no red.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::Green, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let bird = scenario
        .add_creature_from_oracle(P0, "Whippoorwill", 1, 1, WHIPPOORWILL)
        .id();
    let survivor = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();

    let mut runner = scenario.build();

    runner.activate(bird, 0).target_object(survivor).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the no-kill arm must install the same delayed trigger"
    );

    // NOT the drain helper: this trigger is SUPPOSED never to fire, so waiting
    // for the delayed-trigger list to empty would stall rather than measure.
    advance_past_end_of_turn(&mut runner);

    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "T-D1 arm 2: a referent that never died must be untouched — the trigger's \
         condition was never met"
    );
}

// ================================================= T-Z5 (PLACEMENT DETECTOR)

/// T-Z5 — **THE §5.5(e) DETECTOR** for the `counters.rs` guarded terminal read.
/// Role: PLACEMENT DETECTOR.
///
/// This is the FIRST executed test of that seam. Plan round 8 claimed `T-Z4`
/// (`lagrella`) covered it; round 1 measured that Lagrella never reaches the
/// line — her `PutCounter` returns earlier, at the ungated `resolve_event_
/// context_targets` arm. The seam was guarded but untested, not mis-tested.
///
/// `cycle of life` was chosen from the ten pinned counters-family pairs because
/// its `count` is `Fixed(1)`. The rejected alternatives and why:
/// `sacred boon` / `scars of the veteran` use `EventContextAmount` counts, which
/// would make a red ambiguous between "no counter" and "a differently-sized
/// counter"; `side quest` is an Un-set card; `infinite authority` carries an
/// intervening-if that adds a second failure mode.
///
/// Verified at the card data: root `GenericEffect` with a real
/// `target: Typed { type_filters: [Creature] }` slot, delayed
/// `AtNextPhaseForPlayer { phase: Upkeep }` → `PutCounter { counter_type: P1P1,
/// count: Fixed(1), target: ParentTarget }`, `uses_tracked_set: false`.
///
/// **Revert-failing:** revert §5.5(e)'s substitution and the blink arm places
/// the counter anyway, turning this test red.
#[test]
fn t_z5_cycle_of_life_places_no_counter_on_a_blinked_referent() {
    // Verbatim Oracle text (Scryfall).
    const CYCLE_OF_LIFE: &str = "Return this enchantment to its owner's hand: Target creature you cast this turn has base power and toughness 0/1 until your next upkeep. At the beginning of your next upkeep, put a +1/+1 counter on that creature.";

    // The `AtNextPhaseForPlayer { Upkeep }` condition does NOT name the
    // referent's zone change, so this card IS pinned — the opposite verdict from
    // T-D1's `WhenDies`, through the same gate. That contrast is deliberate.
    fn counters_on(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
        runner.state().objects[&id]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    // ---- Arm 1 (MANDATORY reach-guard): no blink, the counter IS placed. ----
    // Without this arm, "no counter" in arm 2 would pass vacuously on a trigger
    // that never fired at all.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::White, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let cycle = scenario
        .add_enchantment_from_oracle(P0, "Cycle of Life", CYCLE_OF_LIFE)
        .id();
    let subject = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    runner.activate(cycle, 0).target_object(subject).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: activating Cycle of Life must install exactly one delayed trigger"
    );

    advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        counters_on(&runner, subject),
        1,
        "T-Z5 arm 1: with no blink the delayed upkeep trigger must place its \
         +1/+1 counter — this is the reach-guard that makes arm 2 non-vacuous"
    );

    // ---- Arm 2 (the detector): blink the referent, no counter may land. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, [mana_of(ManaType::White, 4), mana(8)].concat());
    stock_libraries(&mut scenario);

    let cycle = scenario
        .add_enchantment_from_oracle(P0, "Cycle of Life", CYCLE_OF_LIFE)
        .id();
    let subject = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .id();

    let mut runner = scenario.build();

    runner.activate(cycle, 0).target_object(subject).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the blink arm must install the same delayed trigger"
    );

    let blinked = runner.cast(ephemerate).target_object(subject).resolve();
    // Reach-guard: the blink really happened and the creature really came back,
    // so "no counter" cannot pass on a creature that is simply gone.
    assert_eq!(
        blinked.zone_of(subject),
        Zone::Battlefield,
        "reach-guard: Ephemerate must return the creature to the battlefield"
    );

    let events = advance_until_delayed_triggers_resolve(&mut runner);

    assert_eq!(
        counters_on(&runner, subject),
        0,
        "T-Z5 arm 2: the blinked referent is a NEW object (CR 400.7), so the \
         pinned delayed trigger must place no counter on it"
    );
    // CR 603.7b: the trigger still fires and still resolves — it just affects
    // nothing. Asserting the no-op WITHOUT this would also pass on a fix that
    // wrongly suppressed the trigger entirely.
    assert!(
        effect_resolved_from(&events, cycle),
        "T-Z5 arm 2: the delayed trigger must still have fired and resolved as a \
         no-op (CR 603.7b)"
    );
}
