//! Source-pronoun damage tails and complete counter-clause ownership.
//!
//! The two complete Oracle faces and their metadata were independently read
//! from the supplied MTGJSON-derived export (uncompressed SHA256
//! 9cf481fb0a2f50d431e45d57a112bc35b71f87fe9499627aae4a450eb5b828a9).
//! Every ability below is freshly parsed; no archived ability tree is reused.

use std::ops::ControlFlow;
use std::sync::Arc;

use engine::database::card_db::CardDbHandle;
use engine::database::synthesis::synthesize_all;
use engine::database::CardDatabase;
use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::printed_cards::{apply_card_face_to_object, back_face_for_card_face};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Effect, EffectOutcomeSignal, EffectScope, MultiTargetSpec,
    PlayerFilter, PtValue, QuantityExpr, TapStateChange, TargetFilter, TargetRef,
};
use engine::types::ability_visit::visit_ability_def;
use engine::types::actions::GameAction;
use engine::types::card::{CardFace, LayoutKind, PrintedCardRef};
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const ORACLE_ID: &str = "b4872bac-5822-4c35-9b73-38c4e3ffa477";
const MASTER: &str = "Aang, Master of Elements";
const AVATAR: &str = "Avatar Aang";
const MASTER_ORACLE: &str = "Flying\nSpells you cast cost {W}{U}{B}{R}{G} less to cast. (This can reduce generic costs.)\nAt the beginning of each upkeep, you may transform Aang, Master of Elements. If you do, you gain 4 life, draw four cards, put four +1/+1 counters on him, and he deals 4 damage to each opponent.";
const AVATAR_ORACLE: &str = "Flying, firebending 2\nWhenever you waterbend, earthbend, firebend, or airbend, draw a card. Then if you've done all four this turn, transform Avatar Aang.";

fn fresh_face(name: &str) -> CardFace {
    let (text, hints, subtypes, pt, mana_cost, keywords) = match name {
        MASTER => (
            MASTER_ORACLE,
            vec!["Flying".to_owned()],
            vec!["Avatar".to_owned(), "Ally".to_owned()],
            6,
            ManaCost::NoCost,
            vec![Keyword::Flying],
        ),
        AVATAR => (
            AVATAR_ORACLE,
            vec!["Flying".to_owned(), "firebending".to_owned()],
            vec!["Human".to_owned(), "Avatar".to_owned(), "Ally".to_owned()],
            4,
            ManaCost::Cost {
                generic: 0,
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::Green,
                    ManaCostShard::White,
                    ManaCostShard::Blue,
                ],
            },
            vec![
                Keyword::Flying,
                Keyword::Firebending(QuantityExpr::Fixed { value: 2 }),
            ],
        ),
        _ => panic!("not an Aang face"),
    };
    let parsed = parse_oracle_text(text, name, &hints, &["Creature".to_owned()], &subtypes);
    let mut face = CardFace {
        name: name.to_owned(),
        mana_cost,
        card_type: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes,
        },
        power: Some(PtValue::Fixed(pt)),
        toughness: Some(PtValue::Fixed(pt)),
        oracle_text: Some(text.to_owned()),
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        parse_warnings: parsed.parse_warnings,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        strive_cost: parsed.strive_cost,
        color_override: (name == AVATAR).then(|| {
            vec![
                ManaColor::Green,
                ManaColor::Red,
                ManaColor::Blue,
                ManaColor::White,
            ]
        }),
        color_identity: ManaColor::ALL.to_vec(),
        scryfall_oracle_id: Some(ORACLE_ID.to_owned()),
        brawl_commander: true,
        is_commander: true,
        ..Default::default()
    };
    synthesize_all(&mut face);
    face
}

fn fresh_database() -> Arc<CardDatabase> {
    let mut entries = serde_json::Map::new();
    for (index, name) in [AVATAR, MASTER].into_iter().enumerate() {
        let mut face = serde_json::to_value(fresh_face(name)).expect("face serializes");
        face["layout"] = serde_json::json!("transform");
        face["face_index"] = serde_json::json!(index);
        entries.insert(name.to_lowercase(), face);
    }
    Arc::new(
        CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string())
            .expect("fresh two-face export imports"),
    )
}

fn assert_no_gaps(def: &AbilityDefinition) {
    let _ = visit_ability_def(def, &mut |effect| {
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "{effect:?}"
        );
        ControlFlow::Continue(())
    });
}

fn assert_terminal_resolution(state: &GameState) {
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(state.pending_cast.is_none());
    assert!(state.pending_resolution_completion.is_none());
}

fn upkeep(face: &CardFace) -> &AbilityDefinition {
    let triggers: Vec<_> = face
        .triggers
        .iter()
        .filter(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .collect();
    assert_eq!(triggers.len(), 1, "exactly one upkeep instruction");
    triggers[0].execute.as_deref().expect("upkeep body")
}

#[test]
fn aang_full_two_face_oracle_shape_and_import_identity() {
    let db = fresh_database();
    let master = db.get_face_by_name(MASTER).unwrap();
    let avatar = db.get_face_by_name(AVATAR).unwrap();
    assert_eq!(master.oracle_text.as_deref(), Some(MASTER_ORACLE));
    assert_eq!(avatar.oracle_text.as_deref(), Some(AVATAR_ORACLE));
    for face in [master, avatar] {
        assert!(
            face.parse_warnings.is_empty(),
            "{}: {:?}",
            face.name,
            face.parse_warnings
        );
        for trigger in &face.triggers {
            if let Some(def) = trigger.execute.as_deref() {
                assert_no_gaps(def);
            }
        }
        for def in &face.abilities {
            assert_no_gaps(def);
        }
    }
    assert_eq!(db.get_layout_kind(ORACLE_ID), Some(LayoutKind::Transform));
    assert_eq!(db.get_face_by_oracle_id(ORACLE_ID).unwrap().name, AVATAR);
    let reference = PrintedCardRef {
        oracle_id: ORACLE_ID.to_owned(),
        face_name: MASTER.to_owned(),
    };
    assert_eq!(
        db.get_other_face_by_printed_ref(&reference).unwrap().name,
        AVATAR
    );
    assert_eq!(
        master.static_abilities.len(),
        1,
        "full cost reduction remains represented"
    );
    assert!(avatar
        .keywords
        .contains(&Keyword::Firebending(QuantityExpr::Fixed { value: 2 })));
    assert!(avatar
        .triggers
        .iter()
        .any(|t| t.mode == TriggerMode::ElementalBend));
    let chain: Vec<_> =
        std::iter::successors(Some(upkeep(master)), |def| def.sub_ability.as_deref()).collect();
    assert_eq!(
        chain.len(),
        5,
        "optional transform plus all four payoffs: {chain:?}"
    );
    assert!(chain[0].optional);
    assert!(matches!(
        *chain[0].effect,
        Effect::Transform {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single
        }
    ));
    assert!(matches!(
        *chain[1].effect,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller
        }
    ));
    assert!(matches!(
        *chain[2].effect,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 4 },
            target: TargetFilter::Controller
        }
    ));
    assert!(matches!(
        *chain[3].effect,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 4 },
            target: TargetFilter::SelfRef
        }
    ));
    assert!(matches!(
        *chain[4].effect,
        Effect::DamageEachPlayer {
            amount: QuantityExpr::Fixed { value: 4 },
            player_filter: PlayerFilter::Opponent,
            ..
        }
    ));
    // CR 118.12: every payoff checks the same resolution-time optional cost.
    for def in chain.iter().skip(1) {
        assert_eq!(
            def.condition,
            Some(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed
            })
        );
    }
}

struct UpkeepFixture {
    runner: GameRunner,
    source: ObjectId,
    others: [ObjectId; 2],
    response: ObjectId,
}

fn setup_upkeep() -> UpkeepFixture {
    let db = fresh_database();
    let mut scenario = GameScenario::new_n_player(3, 42);
    // Transition from the end step avoids unrelated combat/turn prompts.
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five", "Six"]);
    scenario.with_library_top(P1, &["Opponent card"]);
    scenario.with_library_top(P2, &["Other opponent card"]);
    let source = scenario.add_creature(P0, MASTER, 6, 6).id();
    let other_source = scenario.add_creature(P1, AVATAR, 4, 4).id();
    let other_creature = scenario.add_creature(P2, "Unrelated creature", 3, 3).id();
    let prior_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Earlier target", true, "Tap target creature.")
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P1, "Transform response", true, "Transform target creature.")
        .id();
    let mut runner = scenario.build();
    for (id, name) in [(source, MASTER), (other_source, AVATAR)] {
        let face = db.get_face_by_name(name).unwrap();
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        apply_card_face_to_object(obj, face);
        obj.back_face = back_face_for_card_face(&db, face);
        obj.transformed = name == MASTER;
    }
    runner.state_mut().card_db = Some(CardDbHandle::new(db));
    runner
        .cast(prior_spell)
        .target_object(other_creature)
        .resolve();
    assert!(
        runner.state().objects[&other_creature].tapped,
        "unrelated earlier target really resolved"
    );
    UpkeepFixture {
        runner,
        source,
        others: [other_source, other_creature],
        response,
    }
}

fn begin_upkeep(f: &mut UpkeepFixture) {
    for _ in 0..12 {
        if f.runner.state().phase == Phase::Upkeep {
            break;
        }
        assert!(matches!(
            f.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        f.runner
            .act(GameAction::PassPriority)
            .expect("advance normally to upkeep");
    }
    assert_eq!(f.runner.state().phase, Phase::Upkeep);
    let entries: Vec<_> = f
        .runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == f.source)
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "reach guard: the actual upkeep trigger is on the stack"
    );
    assert!(matches!(
        entries[0].kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_eq!(entries[0].controller, P0);
    assert!(
        entries[0].ability().unwrap().targets.is_empty(),
        "untargeted source and each opponent"
    );
}

fn finish_upkeep(f: &mut UpkeepFixture, answer: Option<bool>) -> Vec<GameEvent> {
    let mut choices = 0;
    let mut events = Vec::new();
    for _ in 0..20 {
        match f.runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!((player, source_id), (P0, f.source));
                choices += 1;
                assert_eq!(choices, 1, "only the transform cost is optional");
                let accept = answer.expect("an impossible transform must auto-decline");
                events.extend(
                    f.runner
                        .act(GameAction::DecideOptionalEffect { accept })
                        .expect("answer actual source choice")
                        .events,
                );
            }
            WaitingFor::Priority { .. } if f.runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => events.extend(
                f.runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve trigger")
                    .events,
            ),
            other => panic!("unexpected choice, including target selection: {other:?}"),
        }
    }
    assert_eq!(
        choices,
        usize::from(answer.is_some()),
        "positive optional-choice reach"
    );
    assert!(f.runner.state().stack.is_empty(), "trigger must finish");
    assert_eq!(
        f.runner.state().phase,
        Phase::Upkeep,
        "stop before normal draw"
    );
    events
}

#[test]
fn aang_accepts_full_upkeep_and_counters_source_before_damaging_both_opponents() {
    let mut f = setup_upkeep();
    begin_upkeep(&mut f);
    let hand_before = f.runner.state().players[0].hand.len();
    let events = finish_upkeep(&mut f, Some(true));
    let state = f.runner.state();
    // CR 701.27a + CR 201.5: the same permanent changes face and remains the source.
    assert_eq!(state.objects[&f.source].name, AVATAR);
    assert!(!state.objects[&f.source].transformed);
    assert_eq!(state.objects[&f.source].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&f.source]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    // CR 122.1a: the four counters modify the actual 4/4 Avatar face.
    assert_eq!(
        (
            state.objects[&f.source].power,
            state.objects[&f.source].toughness
        ),
        (Some(8), Some(8))
    );
    assert!(state.objects[&f.source]
        .keywords
        .contains(&Keyword::Firebending(QuantityExpr::Fixed { value: 2 })));
    // CR 109.5 + CR 120.3a: the trigger controller gets the rewards; both opponents take damage.
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        vec![24, 16, 16]
    );
    assert_eq!(state.players[0].hand.len() - hand_before, 4);
    for id in f.others {
        assert!(state.objects[&id].counters.is_empty());
    }
    let counter_index = events.iter().position(|event| matches!(event,
        GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 4, actor } if *object_id == f.source && *actor == P0
    )).expect("source counters were added");
    let damages: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(player),
                amount: 4,
                is_combat: false,
                ..
            } => Some((index, *source_id, *player)),
            _ => None,
        })
        .collect();
    assert_eq!(damages.len(), 2);
    for player in [P1, P2] {
        // CR 608.2c + CR 120.1: counters precede damage from this exact source.
        assert!(damages
            .iter()
            .any(|(index, source, recipient)| *index > counter_index
                && *source == f.source
                && *recipient == player));
    }
}

fn assert_no_payoffs(f: &UpkeepFixture, hand_before: usize, events: &[GameEvent]) {
    let state = f.runner.state();
    assert_eq!(state.objects[&f.source].name, MASTER);
    assert!(state.objects[&f.source].transformed);
    assert!(state.objects[&f.source].counters.is_empty());
    assert_eq!(state.players[0].hand.len(), hand_before);
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        vec![20, 20, 20]
    );
    assert!(!events.iter().any(|e| matches!(
        e,
        GameEvent::CounterAdded { .. } | GameEvent::DamageDealt { .. }
    )));
    for id in f.others {
        assert!(state.objects[&id].counters.is_empty());
    }
}

#[test]
fn aang_declining_reached_optional_transform_has_no_payoffs() {
    let mut f = setup_upkeep();
    begin_upkeep(&mut f);
    let hand_before = f.runner.state().players[0].hand.len();
    let events = finish_upkeep(&mut f, Some(false));
    // CR 118.12: declining the optional transform closes every payoff gate.
    assert_no_payoffs(&f, hand_before, &events);
}

#[test]
fn aang_impossible_transform_auto_declines_after_the_trigger_reaches_stack() {
    let mut f = setup_upkeep();
    let mut export = serde_json::to_value(fresh_face(MASTER)).unwrap();
    export["layout"] = serde_json::json!("transform");
    export["face_index"] = serde_json::json!(1);
    let entries = serde_json::Map::from_iter([(MASTER.to_lowercase(), export)]);
    let single = Arc::new(
        CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string()).unwrap(),
    );
    assert!(
        back_face_for_card_face(&single, single.get_face_by_name(MASTER).unwrap()).is_none(),
        "a single-face export cannot invent an opposite face"
    );
    f.runner.state_mut().card_db = Some(CardDbHandle::new(single));
    f.runner
        .state_mut()
        .objects
        .get_mut(&f.source)
        .unwrap()
        .back_face = None;
    begin_upkeep(&mut f);
    assert!(f.runner.state().objects[&f.source].back_face.is_none());
    let hand_before = f.runner.state().players[0].hand.len();
    let events = finish_upkeep(&mut f, None);
    // CR 608.2d + CR 701.27c: an impossible transform cannot be chosen as a cost.
    assert_no_payoffs(&f, hand_before, &events);
}

#[test]
fn aang_stale_transform_auto_declines_after_a_response_changes_the_same_source() {
    let mut f = setup_upkeep();
    begin_upkeep(&mut f);
    let hand_before = f.runner.state().players[0].hand.len();
    let generation = f.runner.state().objects[&f.source].transformation_count;
    // Commit a normal targeted response, then resolve only that top spell.
    drop(f.runner.cast(f.response).target_object(f.source).commit());
    for _ in 0..6 {
        if f.runner.state().stack.len() == 1 {
            break;
        }
        assert!(matches!(
            f.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        f.runner
            .act(GameAction::PassPriority)
            .expect("resolve transform response");
    }
    assert_eq!(f.runner.state().stack.len(), 1);
    assert_eq!(f.runner.state().objects[&f.source].name, AVATAR);
    assert_eq!(
        f.runner.state().objects[&f.source].transformation_count,
        generation + 1
    );
    let events = finish_upkeep(&mut f, None);
    // CR 701.27f + CR 608.2d: the stale transform cannot be chosen or fund its payoffs.
    assert_eq!(f.runner.state().objects[&f.source].name, AVATAR);
    assert_eq!(f.runner.state().players[0].hand.len(), hand_before);
    assert_eq!(
        f.runner
            .state()
            .players
            .iter()
            .map(|p| p.life)
            .collect::<Vec<_>>(),
        vec![20, 20, 20]
    );
    assert!(f.runner.state().objects[&f.source].counters.is_empty());
    assert!(!events.iter().any(|e| matches!(
        e,
        GameEvent::CounterAdded { .. } | GameEvent::DamageDealt { .. }
    )));
}

#[test]
fn counter_tail_failure_is_visible_in_full_activated_oracle_and_never_executes_a_head_only_put() {
    for clause in [
        "Put a +1/+1 counter on this creature with frobnicate",
        "Put a flying counter and a vigilance counter on this creature and frobnicate",
        "Put a +1/+1 counter on this creature with that many frobnications",
    ] {
        let oracle = format!("{{0}}: {clause}.");
        let parsed = parse_oracle_text(
            &oracle,
            "Counter witness",
            &[],
            &["Creature".to_owned()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1);
        assert!(
            matches!(parsed.abilities[0].effect.as_ref(), Effect::Unimplemented { name, description } if name == "put_counter_tail" && description.as_deref().map(str::to_lowercase) == Some(clause.to_lowercase().replace("this creature", "~"))),
            "{:?}",
            parsed.abilities
        );
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Counter witness", 3, 3)
            .from_oracle_text(&oracle)
            .id();
        let mut runner = scenario.build();
        assert!(runner.state().stack.is_empty());
        // CR 602.2a: observe the actual activated ability after normal commitment.
        let activation = runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("the untargeted zero-cost activation commits");
        assert!(activation.disposition.is_applied());
        let mut events = activation.events;
        assert_eq!(runner.state().stack.len(), 1, "{clause}");
        let entry = &runner.state().stack[0];
        assert!(matches!(
            &entry.kind,
            StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == source
        ));
        assert_eq!(entry.source_id, source);
        assert_eq!(entry.controller, P0);
        let entry_id = entry.id;
        assert_ne!(
            entry_id, source,
            "the stack object is distinct from its source"
        );
        let ability = entry.ability().expect("the committed activated ability");
        assert_eq!(ability.source_id, source);
        assert_eq!(ability.controller, P0);
        assert!(
            matches!(&ability.effect, Effect::Unimplemented { name, description }
                if name == "put_counter_tail"
                    && description.as_deref().map(str::to_lowercase)
                        == Some(clause.to_lowercase().replace("this creature", "~"))),
            "{clause}: {ability:?}"
        );
        assert!(
            ability.targets.is_empty(),
            "the root gap has no chosen targets"
        );
        assert!(
            ability.sub_ability.is_none(),
            "no counter head survives the root gap"
        );
        for _ in 0..6 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let result = runner
                .act(GameAction::PassPriority)
                .expect("normal gap resolution");
            assert!(result.disposition.is_applied());
            events.extend(result.events);
        }
        assert!(
            events.iter().any(|event| matches!(event,
                GameEvent::StackResolved { object_id } if *object_id == entry_id
            )),
            "{clause}: {events:?}"
        );
        assert_terminal_resolution(runner.state());
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert!(runner.state().objects[&source].counters.is_empty());
        assert!(
            !events.iter().any(|event| matches!(event,
                GameEvent::CounterAdded { object_id, .. } if *object_id == source
            )),
            "{clause}: {events:?}"
        );
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Counter witness", 3, 3)
        .from_oracle_text("{0}: Put a +1/+1 counter on this creature.")
        .id();
    let mut runner = scenario.build();
    let outcome = runner.activate(source, 0).resolve();
    assert_eq!(
        outcome.state().objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor }
            if *object_id == source && *actor == P0
    )));
    assert_terminal_resolution(outcome.state());
}

#[test]
fn fixed_counter_compound_keeps_supported_head_and_explicit_tail_gap_shape() {
    // SHAPE: the earlier compound owner keeps the counter head and an unsupported child.
    for clause in [
        "Put a +1/+1 counter on this creature and frobnicate",
        "Put a +1/+1 counter on this creature and remember that many counters",
    ] {
        let oracle = format!("{{0}}: {clause}.");
        let parsed = parse_oracle_text(
            &oracle,
            "Counter witness",
            &[],
            &["Creature".to_owned()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1, "{clause}");
        let head = &parsed.abilities[0];
        assert!(
            matches!(
                head.effect.as_ref(),
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef
                }
            ),
            "{clause}: {head:?}"
        );
        let tail = head.sub_ability.as_deref().expect("explicit compound tail");
        assert!(
            matches!(tail.effect.as_ref(), Effect::Unimplemented { .. }),
            "{clause}: {tail:?}"
        );
        assert!(tail.sub_ability.is_none(), "{clause}: {tail:?}");
    }
}

#[test]
fn counter_dynamic_suffix_owners_bind_the_complete_production_instruction() {
    // CR 122.1 + CR 608.2c: each quantity rider determines the counters actually placed.
    for (clause, expected) in [
        ("Put +1/+1 counters on this creature equal to its power.", 3),
        ("Put a +1/+1 counter on this creature for each creature your opponents control.", 2),
        ("Put X +1/+1 counters on this creature, where X is the number of creatures your opponents control.", 2),
    ] {
        let oracle = format!("{{0}}: {clause}");
        let parsed = parse_oracle_text(&oracle, "Counter witness", &[], &["Creature".to_owned()], &[]);
        assert_eq!(parsed.abilities.len(), 1, "{clause}");
        assert_no_gaps(&parsed.abilities[0]);
        assert!(parsed.parse_warnings.is_empty(), "{clause}: {:?}", parsed.parse_warnings);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature(P0, "Counter witness", 3, 3).from_oracle_text(&oracle).id();
        scenario.add_creature(P1, "First opponent creature", 2, 2);
        scenario.add_creature(P1, "Second opponent creature", 2, 2);
        let mut runner = scenario.build();
        runner.activate(source, 0).resolve();
        assert_eq!(runner.state().objects[&source].counters.get(&CounterType::Plus1Plus1), Some(&expected), "{clause}");
    }
}

#[test]
fn source_counter_lists_keep_all_entries_on_source_without_damage() {
    // Synthetic grammar/authority witnesses, independent of a damage boundary.
    for (oracle, kinds) in [
        (
            "{0}: Put a flying counter and a vigilance counter on this creature.",
            vec![KeywordKind::Flying, KeywordKind::Vigilance],
        ),
        (
            "{0}: Put a flying counter, a vigilance counter, and a lifelink counter on this creature.",
            vec![KeywordKind::Flying, KeywordKind::Vigilance, KeywordKind::Lifelink],
        ),
    ] {
        let parsed = parse_oracle_text(oracle, "Counter witness", &[], &["Creature".to_owned()], &[]);
        assert_eq!(parsed.abilities.len(), 1, "{oracle}");
        assert!(parsed.parse_warnings.is_empty(), "{oracle}: {:?}", parsed.parse_warnings);
        assert_no_gaps(&parsed.abilities[0]);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature(P0, "Counter witness", 3, 3).from_oracle_text(oracle).id();
        let own_bystander = scenario.add_creature(P0, "Own bystander", 2, 2).id();
        let opposing_bystander = scenario.add_creature(P1, "Opposing bystander", 2, 2).id();
        let mut runner = scenario.build();
        let abilities = &runner.state().objects[&source].abilities;
        assert_eq!(abilities.len(), 1);
        assert_eq!(abilities[0], parsed.abilities[0]);
        let chain: Vec<_> = std::iter::successors(Some(&abilities[0]), |def| def.sub_ability.as_deref()).collect();
        assert_eq!(chain.len(), kinds.len(), "{oracle}: {chain:?}");
        for (def, kind) in chain.iter().zip(&kinds) {
            assert_eq!(def.effect.as_ref(), &Effect::PutCounter {
                counter_type: CounterType::Keyword(*kind),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }, "{oracle}");
        }
        let resolved = build_resolved_from_def(&abilities[0], source, P0);
        // CR 115.10a: affecting the source does not announce it as a target.
        assert!(build_target_slots(runner.state(), &resolved).expect("source list slots").is_empty());
        let outcome = runner.activate(source, 0).resolve();
        let state = outcome.state();
        let counters = &state.objects[&source].counters;
        assert_eq!(counters.len(), kinds.len(), "{oracle}: {counters:?}");
        // CR 122.1b + CR 608.2c: every list member places its keyword counter on the source.
        for kind in kinds {
            assert_eq!(counters.get(&CounterType::Keyword(kind)), Some(&1), "{oracle}: {kind:?}, {counters:?}, {:?}", outcome.events());
            assert!(outcome.events().iter().any(|event| matches!(event,
                GameEvent::CounterAdded { object_id, counter_type, count: 1, actor }
                    if *object_id == source && *counter_type == CounterType::Keyword(kind) && *actor == P0
            )), "{oracle}: {kind:?}, {:?}", outcome.events());
        }
        for bystander in [own_bystander, opposing_bystander] {
            assert!(state.objects[&bystander].counters.is_empty(), "{oracle}");
        }
        outcome.assert_life_delta(P0, 0);
        outcome.assert_life_delta(P1, 0);
        assert_terminal_resolution(state);
    }
}

#[test]
fn source_counter_list_ignores_an_earlier_distinct_chosen_target() {
    // Synthetic authority witness: the earlier chosen creature is not the source.
    let oracle =
        "{0}: Tap target creature. Put a flying counter and a vigilance counter on this creature.";
    let parsed = parse_oracle_text(
        oracle,
        "Counter witness",
        &[],
        &["Creature".to_owned()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );
    assert_no_gaps(&parsed.abilities[0]);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Counter witness", 3, 3)
        .from_oracle_text(oracle)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen creature", 2, 2).id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let mut runner = scenario.build();
    let abilities = &runner.state().objects[&source].abilities;
    assert_eq!(abilities.len(), 1);
    assert_eq!(abilities[0], parsed.abilities[0]);
    let chain: Vec<_> =
        std::iter::successors(Some(&abilities[0]), |def| def.sub_ability.as_deref()).collect();
    assert_eq!(chain.len(), 3, "{chain:?}");
    assert!(
        matches!(
            chain[0].effect.as_ref(),
            Effect::SetTapState {
                target: TargetFilter::Typed(_),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ),
        "{chain:?}"
    );
    for (def, kind) in chain
        .iter()
        .skip(1)
        .zip([KeywordKind::Flying, KeywordKind::Vigilance])
    {
        assert_eq!(
            def.effect.as_ref(),
            &Effect::PutCounter {
                counter_type: CounterType::Keyword(kind),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }
        );
    }
    let resolved = build_resolved_from_def(&abilities[0], source, P0);
    // CR 601.2c + CR 115.10a: only the first instruction announces a creature target.
    assert_eq!(
        build_target_slots(runner.state(), &resolved)
            .expect("tap target slot")
            .len(),
        1
    );
    let outcome = runner.activate(source, 0).target_object(chosen).resolve();
    let state = outcome.state();
    assert!(
        state.objects[&chosen].tapped,
        "the earlier chosen target was actually used"
    );
    assert!(!state.objects[&source].tapped);
    let counters = &state.objects[&source].counters;
    assert_eq!(counters.len(), 2, "{counters:?}");
    // CR 608.2c: the later explicit-source instruction does not inherit the tap recipient.
    for kind in [KeywordKind::Flying, KeywordKind::Vigilance] {
        assert_eq!(
            counters.get(&CounterType::Keyword(kind)),
            Some(&1),
            "{kind:?}: {counters:?}, {:?}",
            outcome.events()
        );
    }
    for other in [chosen, bystander] {
        assert!(state.objects[&other].counters.is_empty());
    }
    assert_terminal_resolution(state);
}

#[test]
fn chosen_counter_list_reuses_one_announced_recipient() {
    // Unexpected Fangs' complete Oracle body from the pinned MTGJSON export.
    let oracle = "Put a +1/+1 counter and a lifelink counter on target creature.";
    let parsed = parse_oracle_text(
        oracle,
        "Unexpected Fangs",
        &[],
        &["Instant".to_owned()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );
    assert_no_gaps(&parsed.abilities[0]);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Unexpected Fangs", true, oracle)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen creature", 2, 2).id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let mut runner = scenario.build();
    let abilities = &runner.state().objects[&spell].abilities;
    assert_eq!(abilities.len(), 1);
    assert_eq!(abilities[0], parsed.abilities[0]);
    let resolved = build_resolved_from_def(&abilities[0], spell, P0);
    // CR 601.2c: the shared recipient is announced once for the complete list.
    assert_eq!(
        build_target_slots(runner.state(), &resolved)
            .expect("chosen list slot")
            .len(),
        1
    );
    let outcome = runner.cast(spell).target_object(chosen).resolve();
    // CR 122.1 + CR 608.2c: both counters belong to that one chosen creature.
    outcome.assert_counters(chosen, CounterType::Plus1Plus1, 1);
    outcome.assert_counters(chosen, CounterType::Keyword(KeywordKind::Lifelink), 1);
    assert_eq!(outcome.state().objects[&chosen].counters.len(), 2);
    assert!(outcome.state().objects[&bystander].counters.is_empty());
    assert_terminal_resolution(outcome.state());
}

#[test]
fn source_pronoun_damage_executes_after_list_counters_for_each_grammar_axis() {
    for pronoun in ["he", "she"] {
        for verb in ["deal", "deals"] {
            for connector in [", and ", " and "] {
                let oracle = format!("{{0}}: Put a flying counter and a vigilance counter on this creature{connector}{pronoun} {verb} 2 damage to each opponent.");
                let parsed = parse_oracle_text(
                    &oracle,
                    "Counter witness",
                    &[],
                    &["Creature".to_owned()],
                    &[],
                );
                assert_eq!(parsed.abilities.len(), 1);
                assert_no_gaps(&parsed.abilities[0]);
                assert!(
                    parsed.parse_warnings.is_empty(),
                    "{oracle}: {:?}",
                    parsed.parse_warnings
                );
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let source = scenario
                    .add_creature(P0, "Counter witness", 3, 3)
                    .from_oracle_text(&oracle)
                    .id();
                let mut runner = scenario.build();
                let abilities = &runner.state().objects[&source].abilities;
                assert_eq!(abilities.len(), 1);
                assert_eq!(abilities[0], parsed.abilities[0]);
                let chain: Vec<_> =
                    std::iter::successors(Some(&abilities[0]), |def| def.sub_ability.as_deref())
                        .collect();
                assert_eq!(chain.len(), 3, "{oracle}: {chain:?}");
                for (def, kind) in chain
                    .iter()
                    .take(2)
                    .zip([KeywordKind::Flying, KeywordKind::Vigilance])
                {
                    assert_eq!(
                        def.effect.as_ref(),
                        &Effect::PutCounter {
                            counter_type: CounterType::Keyword(kind),
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                        "{oracle}"
                    );
                }
                assert!(
                    matches!(
                        chain[2].effect.as_ref(),
                        Effect::DamageEachPlayer {
                            amount: QuantityExpr::Fixed { value: 2 },
                            player_filter: PlayerFilter::Opponent,
                        }
                    ),
                    "{oracle}: {chain:?}"
                );
                let resolved = build_resolved_from_def(&abilities[0], source, P0);
                assert!(build_target_slots(runner.state(), &resolved)
                    .expect("source damage slots")
                    .is_empty());
                let outcome = runner.activate(source, 0).resolve();
                let obj = &outcome.state().objects[&source];
                let events = outcome.events();
                let damage_index = events.iter().position(|event| matches!(event,
                    GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, is_combat: false, .. }
                        if *source_id == source && *player == P1
                )).expect("the exact source dealt two noncombat damage to P1");
                // CR 122.1b: both printed keyword counters are placed on this source.
                for kind in [KeywordKind::Flying, KeywordKind::Vigilance] {
                    assert_eq!(
                        obj.counters.get(&CounterType::Keyword(kind)),
                        Some(&1),
                        "{oracle}: {kind:?}, counters: {:?}, events: {events:?}",
                        obj.counters
                    );
                    let counter_index = events.iter().position(|event| matches!(event,
                        GameEvent::CounterAdded { object_id, counter_type, count: 1, actor }
                            if *object_id == source && *counter_type == CounterType::Keyword(kind) && *actor == P0
                    )).unwrap_or_else(|| panic!("{oracle}: missing {kind:?} placement, counters: {:?}, events: {events:?}", obj.counters));
                    // CR 608.2c: both actual counter placements precede the damage instruction.
                    assert!(
                        counter_index < damage_index,
                        "{oracle}: {kind:?}, {events:?}"
                    );
                }
                assert_eq!(outcome.state().players[0].life, 20);
                assert_eq!(outcome.state().players[1].life, 18, "{oracle}");
                assert_terminal_resolution(outcome.state());
            }
        }
    }
}

#[test]
fn counter_placement_keeps_mass_and_announced_recipient_cardinality() {
    for text in [
        "Put a +1/+1 counter on each of two target creatures.",
        "Put a +1/+1 counter on each of up to two target creatures.",
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Recipient set", true, text)
            .id();
        let first = scenario.add_creature(P0, "First", 2, 2).id();
        let second = scenario.add_creature(P1, "Second", 2, 2).id();
        let outside = scenario.add_creature(P1, "Outside", 2, 2).id();
        let mut runner = scenario.build();
        let outcome = runner
            .cast(spell)
            .target_objects(&[first, second])
            .resolve();
        // CR 601.2c: the complete announced set, and no other creature, gets counters.
        for id in [first, second] {
            outcome.assert_counters(id, CounterType::Plus1Plus1, 1);
        }
        outcome.assert_counters(outside, CounterType::Plus1Plus1, 0);
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Mass placement",
            true,
            "Put two +1/+1 counters on each creature you control.",
        )
        .id();
    let first = scenario.add_creature(P0, "First", 2, 2).id();
    let second = scenario.add_creature(P0, "Second", 2, 2).id();
    let outside = scenario.add_creature(P1, "Outside", 2, 2).id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    for id in [first, second] {
        outcome.assert_counters(id, CounterType::Plus1Plus1, 2);
    }
    outcome.assert_counters(outside, CounterType::Plus1Plus1, 0);
}

#[test]
fn optional_counter_count_and_target_count_remain_independent_shape() {
    // CR 608.2d + CR 601.2c: optional counter magnitude and announced target count are separate choices.
    let parsed = parse_oracle_text(
        "Put up to three +1/+1 counters on each of up to two target creatures.",
        "Optional placement",
        &[],
        &["Sorcery".to_owned()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let ability = &parsed.abilities[0];
    assert_no_gaps(ability);
    let Effect::PutCounter { count, .. } = ability.effect.as_ref() else {
        panic!("{:?}", ability.effect);
    };
    assert_eq!(
        count.peel_up_to(),
        (&QuantityExpr::Fixed { value: 3 }, true)
    );
    assert_eq!(
        ability.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 2 }))
    );
}

#[test]
fn quoted_source_damage_stays_within_real_granted_ability_shape() {
    let oracle = "Target creature gains \"{T}: Put a +1/+1 counter on this creature and he deals 1 damage to each opponent.\" until end of turn.";
    let parsed = parse_oracle_text(oracle, "Grant witness", &[], &["Instant".to_owned()], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let outer = &parsed.abilities[0];
    assert_no_gaps(outer);
    assert!(outer.sub_ability.is_none(), "damage stays within the grant");
    let mut nested_damage = 0;
    let _ = visit_ability_def(outer, &mut |effect| {
        if matches!(effect, Effect::DamageEachPlayer { .. }) {
            nested_damage += 1;
        }
        ControlFlow::Continue(())
    });
    assert_eq!(
        nested_damage, 1,
        "the quoted damage ability is really parsed"
    );
}
