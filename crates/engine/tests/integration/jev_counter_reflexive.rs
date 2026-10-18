//! U12 mandatory adjustment production delivery. Full Immard's actual-removal
//! reflexive and independent modal stack object are DEFERRED(phase 13).
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
