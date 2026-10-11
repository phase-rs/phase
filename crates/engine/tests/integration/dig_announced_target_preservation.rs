use crate::support::shared_card_db;
use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCondition, DigRestOrder, Effect, LibraryPosition, QuantityExpr, ResolvedAbility,
    SubAbilityLink, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, PendingContinuation, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PEER: &str = "Look at the top five cards of your library. You may reveal an instant or sorcery card from among them and put it into your hand. Put the rest on the bottom of your library in any order.";
const REACH: &str = "Draw a card.";
const GLACIAL: &str = "Glacial Ray deals 2 damage to any target.\nSplice onto Arcane {1}{R} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";

#[test]
fn glacial_ray_after_peer_decline_preserves_announced_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looked = scenario
        .add_spell_to_library_top(P0, "Reach Through Mists", true)
        .from_oracle_text(REACH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let host = scenario
        .add_spell_to_hand_from_oracle(P0, "Peer Through Depths", true, PEER)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Glacial Ray", true, GLACIAL)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&host].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: host,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SpliceOffer { .. }
    ));
    runner
        .act(GameAction::RespondToSpliceOffer { card: Some(ray) })
        .unwrap();
    let WaitingFor::TargetSelection {
        selection,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual target selection: {:?}", runner.state().waiting_for)
    };
    assert_eq!(target_slots.len(), 1);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Player(P1)));
    // CR 702.47d + CR 601.2c: added text chooses its target at announcement.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .unwrap();
    let announced = runner
        .state()
        .stack
        .back()
        .unwrap()
        .ability()
        .unwrap()
        .sub_ability
        .as_ref()
        .unwrap();
    assert!(matches!(announced.effect, Effect::DealDamage { .. }));
    assert_eq!(announced.targets, vec![TargetRef::Player(P1)]);
    assert_eq!(announced.source_id, host);
    println!(
        "glacial_ray_after_peer_decline_preserves_announced_player committed node targets={:?} link={:?} source={:?} controller={:?}",
        announced.targets,
        announced.sub_link,
        announced.source_id,
        announced.controller
    );
    let mut events = Vec::new();
    for _ in 0..4 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "printed Peer must reach DigChoice: {:?}",
            runner.state().waiting_for
        )
    };
    assert!(cards.contains(&looked));
    assert!(selectable_cards.contains(&looked));
    let parked = runner.state().active_ability_continuation().unwrap();
    println!(
        "glacial_ray_after_peer_decline_preserves_announced_player before choice targets={:?} link={:?} source={:?} controller={:?}",
        parked.chain.targets,
        parked.chain.sub_link,
        parked.chain.source_id,
        parked.chain.controller
    );
    assert_eq!(parked.chain.targets, vec![TargetRef::Player(P1)]);
    // CR 608.2c: the optional kept-card choice is independent of the added target.
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .unwrap();
    println!(
        "glacial_ray_after_peer_decline_preserves_announced_player after choice waiting={:?} events={:?} pending={:?}",
        result.waiting_for,
        result.events,
        runner
            .state()
            .active_ability_continuation()
            .map(|p| (&p.chain.targets, p.chain.sub_link))
    );
    events.extend(result.events);
    assert_eq!(runner.state().objects[&looked].zone, Zone::Library);
    assert_eq!(runner.state().objects[&ray].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    println!(
        "glacial_ray_after_peer_decline_preserves_announced_player final lives={:?} events={:?}",
        runner
            .state()
            .players
            .iter()
            .map(|p| p.life)
            .collect::<Vec<_>>(),
        events
    );
    // CR 702.47b/c/d + CR 608.2c: the host resolves first, then its added damage targets P1.
    assert_eq!(runner.state().players[P1.0 as usize].life, 18);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(events.iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == host && *player == P1)));
}

#[test]
fn glacial_ray_after_peer_keep_preserves_announced_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looked = scenario
        .add_spell_to_library_top(P0, "Reach Through Mists", true)
        .from_oracle_text(REACH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let host = scenario
        .add_spell_to_hand_from_oracle(P0, "Peer Through Depths", true, PEER)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Glacial Ray", true, GLACIAL)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&host].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: host,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SpliceOffer { .. }
    ));
    runner
        .act(GameAction::RespondToSpliceOffer { card: Some(ray) })
        .unwrap();
    let WaitingFor::TargetSelection {
        selection,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual target selection: {:?}", runner.state().waiting_for)
    };
    assert_eq!(target_slots.len(), 1);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Player(P1)));
    // CR 702.47d + CR 601.2c: added text chooses its target at announcement.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .unwrap();
    let announced = runner
        .state()
        .stack
        .back()
        .unwrap()
        .ability()
        .unwrap()
        .sub_ability
        .as_ref()
        .unwrap();
    assert!(matches!(announced.effect, Effect::DealDamage { .. }));
    assert_eq!(announced.targets, vec![TargetRef::Player(P1)]);
    assert_eq!(announced.source_id, host);
    println!(
        "glacial_ray_after_peer_keep_preserves_announced_player committed node targets={:?} link={:?} source={:?} controller={:?}",
        announced.targets,
        announced.sub_link,
        announced.source_id,
        announced.controller
    );
    let mut events = Vec::new();
    for _ in 0..4 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "printed Peer must reach DigChoice: {:?}",
            runner.state().waiting_for
        )
    };
    assert!(cards.contains(&looked));
    assert!(selectable_cards.contains(&looked));
    let parked = runner.state().active_ability_continuation().unwrap();
    println!(
        "glacial_ray_after_peer_keep_preserves_announced_player before choice targets={:?} link={:?} source={:?} controller={:?}",
        parked.chain.targets,
        parked.chain.sub_link,
        parked.chain.source_id,
        parked.chain.controller
    );
    assert_eq!(parked.chain.targets, vec![TargetRef::Player(P1)]);
    let actions = legal_actions(runner.state());
    assert!(actions.contains(&GameAction::SelectCards { cards: vec![] }));
    let keep = actions
        .into_iter()
        .find(|action| {
            *action
                == GameAction::SelectCards {
                    cards: vec![looked],
                }
        })
        .expect("the eligible keep is an actual legal AI candidate");
    let encoded = serde_json::to_vec(runner.state()).unwrap();
    let restored: GameState = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        restored.active_ability_continuation().unwrap().chain,
        parked.chain
    );
    *runner.state_mut() = restored;
    // CR 608.2c: the optional kept-card choice is independent of the added target.
    let result = runner.act(keep).unwrap();
    println!(
        "glacial_ray_after_peer_keep_preserves_announced_player after choice waiting={:?} events={:?} pending={:?}",
        result.waiting_for,
        result.events,
        runner
            .state()
            .active_ability_continuation()
            .map(|p| (&p.chain.targets, p.chain.sub_link))
    );
    events.extend(result.events);
    assert_eq!(runner.state().objects[&looked].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&ray].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    println!(
        "glacial_ray_after_peer_keep_preserves_announced_player final lives={:?} events={:?}",
        runner
            .state()
            .players
            .iter()
            .map(|p| p.life)
            .collect::<Vec<_>>(),
        events
    );
    // CR 702.47b/c/d + CR 608.2c: the host resolves first, then its added damage targets P1.
    assert_eq!(runner.state().players[P1.0 as usize].life, 18);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(events.iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == host && *player == P1)));
}

#[test]
fn glacial_ray_after_reach_draw_preserves_announced_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looked = scenario
        .add_spell_to_library_top(P0, "Reach Through Mists", true)
        .from_oracle_text(REACH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let host = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, REACH)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Glacial Ray", true, GLACIAL)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&host].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: host,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SpliceOffer { .. }
    ));
    runner
        .act(GameAction::RespondToSpliceOffer { card: Some(ray) })
        .unwrap();
    let WaitingFor::TargetSelection {
        selection,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual target selection: {:?}", runner.state().waiting_for)
    };
    assert_eq!(target_slots.len(), 1);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Player(P1)));
    // CR 702.47d + CR 601.2c: added text chooses its target at announcement.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .unwrap();
    let announced = runner
        .state()
        .stack
        .back()
        .unwrap()
        .ability()
        .unwrap()
        .sub_ability
        .as_ref()
        .unwrap();
    assert!(matches!(announced.effect, Effect::DealDamage { .. }));
    assert_eq!(announced.targets, vec![TargetRef::Player(P1)]);
    assert_eq!(announced.source_id, host);
    println!(
        "glacial_ray_after_reach_draw_preserves_announced_player committed node targets={:?} link={:?} source={:?} controller={:?}",
        announced.targets,
        announced.sub_link,
        announced.source_id,
        announced.controller
    );
    let mut events = Vec::new();
    for _ in 0..4 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().objects[&looked].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&ray].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    println!(
        "glacial_ray_after_reach_draw_preserves_announced_player final lives={:?} events={:?}",
        runner
            .state()
            .players
            .iter()
            .map(|p| p.life)
            .collect::<Vec<_>>(),
        events
    );
    // CR 702.47b/c/d + CR 608.2c: the host resolves first, then its added damage targets P1.
    assert_eq!(runner.state().players[P1.0 as usize].life, 18);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(events.iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == host && *player == P1)));
}

#[test]
fn whiskervale_real_target_trigger_uses_direct_dig_choice_result() {
    const WHISKERVALE: &str = "Valiant — Whenever this creature becomes the target of a spell or ability you control for the first time each turn, look at the top five cards of your library. You may reveal a creature card with mana value 3 or less from among them. You may put it onto the battlefield if it's your turn. If you don't put it onto the battlefield, put it into your hand. Put the rest on the bottom of your library in a random order.";
    let db = shared_card_db().unwrap();
    for put_onto_battlefield in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let forerunner = scenario
            .add_creature_from_oracle(P0, "Whiskervale Forerunner", 3, 4, WHISKERVALE)
            .with_subtypes(vec!["Mouse", "Bard"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            })
            .id();
        let rest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
        let chosen = scenario.add_real_card(P0, "Llanowar Elves", Zone::Library, db);
        let growth = scenario.add_real_card(P0, "Giant Growth", Zone::Hand, db);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        runner.cast(growth).target_object(forerunner).resolve();
        let WaitingFor::DigChoice {
            cards,
            selectable_cards,
            kept_destination,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real Whiskervale trigger: {:?}", runner.state().waiting_for);
        };
        assert_eq!(*kept_destination, None);
        assert!(cards.contains(&rest));
        assert!(selectable_cards.contains(&chosen));
        let mut node = Some(
            runner
                .state()
                .active_ability_continuation()
                .unwrap()
                .chain
                .as_ref(),
        );
        let mut tracked_bottom = false;
        while let Some(current) = node {
            tracked_bottom |= matches!(
                &current.effect,
                Effect::PutAtLibraryPosition {
                    target: engine::types::ability::TargetFilter::TrackedSet { .. },
                    ..
                }
            );
            node = current.sub_ability.as_deref();
        }
        assert!(
            tracked_bottom,
            "None destination plus tracked-bottom descendant reaches direct writer"
        );
        let result = runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap();
        println!(
            "WHISKERVALE_DIRECT option={put_onto_battlefield} after_select={:?} events={:?}",
            result.waiting_for, result.events
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { decision_subject_id: Some(id), source_id, .. } if id == chosen && source_id == forerunner
        ));
        // CR 608.2c: this turn's optional battlefield delivery precedes the Hand fallback.
        runner
            .act(GameAction::DecideOptionalEffect {
                accept: put_onto_battlefield,
            })
            .unwrap();
        println!(
            "WHISKERVALE_DIRECT settled={:?} selected={:?} rest={:?}",
            runner.state().waiting_for,
            runner.state().objects[&chosen].zone,
            runner.state().players[0].library
        );
        assert_eq!(
            runner.state().objects[&chosen].zone,
            if put_onto_battlefield {
                Zone::Battlefield
            } else {
                Zone::Hand
            }
        );
        assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
        assert!(runner.state().players[0].library.contains(&rest));
        assert!(!runner.state().players[0].library.contains(&chosen));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn collected_company_real_copy_replacement_pauses_dig_delivery() {
    let db = shared_card_db().unwrap();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let rest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let image = scenario.add_real_card(P0, "Phantasmal Image", Zone::Library, db);
    let company = scenario.add_real_card(P0, "Collected Company", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.cast(company).resolve();
    let WaitingFor::DigChoice {
        selectable_cards,
        kept_destination,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("real Company: {:?}", runner.state().waiting_for);
    };
    assert_eq!(*kept_destination, Some(Zone::Battlefield));
    assert!(selectable_cards.contains(&image));
    let selected = runner
        .act(GameAction::SelectCards { cards: vec![image] })
        .unwrap();
    println!(
        "COMPANY_PAUSE waiting={:?} events={:?} active={:?}",
        selected.waiting_for,
        selected.events,
        runner.state().active_ability_continuation()
    );
    // CR 614.1c + CR 614.12a: entering as a copy requires a choice before delivery.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().objects[&image].zone, Zone::Library);
    let parked = runner
        .state()
        .active_batch_delivery()
        .expect("real kept delivery must be parked");
    assert!(
        matches!(&parked.completion, Some(engine::types::game_state::BatchCompletion::RevealRestPile { publish_tracked_set: Some(ids), .. }) if ids == &vec![image])
    );
    assert!(runner
        .state()
        .tracked_object_sets
        .values()
        .all(|ids| !ids.contains(&image)));
    println!("COMPANY_PAUSE completion={:?}", parked.completion);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CopyTargetChoice { .. }
    ));
    let settled = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
    println!(
        "COMPANY_PAUSE settled={:?} events={:?} tracked={:?}",
        settled.waiting_for,
        settled.events,
        runner.state().tracked_object_sets
    );
    assert_eq!(runner.state().objects[&image].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&image].name, "Grizzly Bears");
    assert!(runner.state().objects[&image]
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Illusion"));
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
    assert!(runner.state().players[0].library.contains(&rest));
    assert!(!runner.state().players[0].library.contains(&image));
    assert!(runner
        .state()
        .tracked_object_sets
        .values()
        .any(|ids| ids.contains(&image)));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

const KODAMA: &str = "Target creature gets +2/+2 until end of turn.\nSplice onto Arcane {G} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";

#[test]
fn peer_preserves_object_and_independent_splice_targets_and_rechecks_departed_objects() {
    let db = shared_card_db().unwrap();
    for (keep, add_ray, remove_target) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
        (true, true, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        let sibling = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        let looked = scenario
            .add_spell_to_library_top(P0, "Reach Through Mists", true)
            .from_oracle_text(REACH)
            .id();
        let host = scenario
            .add_spell_to_hand_from_oracle(P0, "Peer Through Depths", true, PEER)
            .with_subtypes(vec!["Arcane"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            })
            .id();
        let might = scenario
            .add_spell_to_hand_from_oracle(P0, "Kodama's Might", true, KODAMA)
            .id();
        let ray = add_ray.then(|| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Glacial Ray", true, GLACIAL)
                .id()
        });
        let bounce = remove_target.then(|| scenario.add_real_card(P0, "Unsummon", Zone::Hand, db));
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&host].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: host,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::SpliceOffer { .. }
        ));
        if let Some(ray) = ray {
            runner
                .act(GameAction::RespondToSpliceOffer { card: Some(ray) })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::SpliceOffer { .. }
            ));
        }
        runner
            .act(GameAction::RespondToSpliceOffer { card: Some(might) })
            .unwrap();
        if add_ray {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                })
                .unwrap();
        }
        // CR 601.2c + CR 702.47d: each added target is announced independently.
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
        let committed = runner.state().stack.back().unwrap().ability().unwrap();
        let first = committed.sub_ability.as_deref().unwrap();
        let pump = if add_ray {
            assert_eq!(first.targets, vec![TargetRef::Player(P1)]);
            first.sub_ability.as_deref().unwrap()
        } else {
            first
        };
        assert_eq!(pump.targets, vec![TargetRef::Object(target)]);
        assert_eq!(pump.target_pins.len(), 1);
        let original_pin = pump.target_pins[0].expect("announced pin");
        let mut events = Vec::new();
        if let Some(bounce) = bounce {
            let outcome = runner.cast(bounce).target_object(target).resolve();
            outcome.assert_zone(&[target], Zone::Hand);
            events.extend_from_slice(outcome.events());
            assert_ne!(
                runner.state().objects[&target].incarnation,
                original_pin.incarnation
            );
        }
        for _ in 0..6 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                || runner.state().stack.is_empty()
            {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        if remove_target && !add_ray {
            // CR 608.2b: all targets illegal means the host does not resolve.
            assert!(!matches!(
                runner.state().waiting_for,
                WaitingFor::DigChoice { .. }
            ));
            assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&looked].zone, Zone::Library);
            assert!(events.iter().any(
                |e| matches!(e, GameEvent::StackResolved { object_id } if *object_id == host)
            ));
        } else {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::DigChoice { .. }
            ));
            events.extend(
                runner
                    .act(GameAction::SelectCards {
                        cards: if keep { vec![looked] } else { vec![] },
                    })
                    .unwrap()
                    .events,
            );
            assert_eq!(
                runner.state().objects[&looked].zone,
                if keep { Zone::Hand } else { Zone::Library }
            );
            if add_ray {
                assert_eq!(runner.state().players[1].life, 18);
                assert!(events.iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == host && *player == P1)));
                let damage_events: Vec<_> = events.iter().enumerate().filter(|(_, event)| matches!(event, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == host && *player == P1)).collect();
                assert_eq!(damage_events.len(), 1);
                if !remove_target {
                    let pump_events: Vec<_> = events.iter().enumerate().filter(|(_, event)| matches!(event, GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::Pump, source_id, .. } if *source_id == host)).collect();
                    assert_eq!(pump_events.len(), 1);
                    // CR 702.47b + CR 608.2c: chosen splice order is damage, then pump.
                    assert!(damage_events[0].0 < pump_events[0].0);
                }
            }
        }
        // CR 608.2b: an illegal pump target is unaffected; its legal sibling guards reach.
        assert_eq!(
            runner.state().objects[&target].power,
            Some(if remove_target { 2 } else { 4 })
        );
        assert_eq!(
            runner.state().objects[&target].toughness,
            Some(if remove_target { 2 } else { 4 })
        );
        assert_eq!(runner.state().objects[&sibling].power, Some(2));
        assert_eq!(runner.state().objects[&sibling].toughness, Some(2));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

// Supplemental API contracts: pending state is constructed using the same
// continuation APIs as the existing integration suites, then real actions run.
// These are not printed-card announcement proofs.
#[test]
fn direct_dig_action_preserves_own_target_and_binds_current_conditional_result() {
    let db = shared_card_db().unwrap();
    for own_target in [false, true] {
        for keep in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let stale = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
            let rest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
            let chosen = scenario.add_real_card(P0, "Llanowar Elves", Zone::Library, db);
            let source = scenario.add_real_card(P0, "Collected Company", Zone::Hand, db);
            let mut runner = scenario.build();
            let mut tail = ResolvedAbility::new(
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    count: QuantityExpr::Fixed {
                        value: if keep { 1 } else { 2 },
                    },
                    position: LibraryPosition::Bottom,
                },
                vec![],
                source,
                P0,
            );
            tail.sub_link = SubAbilityLink::SequentialSibling;
            let mut head = if own_target {
                ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::Player,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Player(P1)],
                    source,
                    P0,
                )
            } else {
                ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                        player: TargetFilter::Controller,
                    },
                    vec![TargetRef::Object(stale)],
                    source,
                    P0,
                )
                .condition(AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                    use_lki: false,
                    subject_slot: None,
                })
            }
            .sub_ability(tail);
            head.capture_target_incarnations_recursive(runner.state());
            let expected_targets = head.targets.clone();
            let expected_pins = head.aligned_target_pins();
            let state = runner.state_mut();
            state.park_ability_continuation(PendingContinuation::new(Box::new(head), state));
            state.waiting_for = WaitingFor::DigChoice {
                player: P0,
                library_owner: P0,
                cards: vec![chosen, rest],
                keep_count: 1,
                up_to: true,
                selectable_cards: vec![chosen],
                kept_destination: None,
                rest_destination: Some(Zone::Library),
                rest_split_top_count: None,
                rest_order: DigRestOrder::Preserve,
                source_id: Some(source),
                enter_tapped: false,
                enters_attacking: false,
            };
            let pending = runner.state().active_ability_continuation().unwrap();
            assert_eq!(pending.chain.targets, expected_targets);
            assert_eq!(pending.chain.aligned_target_pins(), expected_pins);
            assert!(matches!(
                &pending.chain.sub_ability.as_ref().unwrap().effect,
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::TrackedSet { .. },
                    ..
                }
            ));
            assert!(
                matches!(&runner.state().waiting_for, WaitingFor::DigChoice { kept_destination: None, selectable_cards, .. } if selectable_cards == &vec![chosen])
            );
            let mut events = runner
                .act(GameAction::SelectCards {
                    cards: if keep { vec![chosen] } else { vec![] },
                })
                .unwrap()
                .events;
            if let WaitingFor::EffectZoneChoice { cards, .. } = &runner.state().waiting_for {
                let cards = cards.clone();
                events.extend(
                    runner
                        .act(GameAction::SelectCards { cards })
                        .unwrap()
                        .events,
                );
            }
            // CR 601.2c: the selected result is independent of the P1 declaration.
            assert_eq!(
                runner.state().players[1].life,
                if own_target { 18 } else { 20 }
            );
            assert_eq!(
                runner.state().players[0].life,
                if !own_target && keep { 23 } else { 20 }
            );
            if own_target {
                assert!(events.iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == source && *player == P1)));
            }
            assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
            assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::PutAtLibraryPosition, source_id, .. } if *source_id == source)));
            assert!(runner.state().active_ability_continuation().is_none());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}

#[test]
fn paused_dig_action_preserves_own_target_and_binds_settled_result() {
    let db = shared_card_db().unwrap();
    for own_target in [false, true] {
        for keep in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let copy_target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
            let rest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
            let image = scenario.add_real_card(P0, "Phantasmal Image", Zone::Library, db);
            let source = scenario.add_real_card(P0, "Collected Company", Zone::Hand, db);
            let mut runner = scenario.build();
            let mut head = if own_target {
                ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::Player,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Player(P1)],
                    source,
                    P0,
                )
            } else {
                ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                        player: TargetFilter::Controller,
                    },
                    vec![TargetRef::Object(copy_target)],
                    source,
                    P0,
                )
                .condition(AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                    use_lki: false,
                    subject_slot: None,
                })
            };
            head.capture_target_incarnations_recursive(runner.state());
            let targets = head.targets.clone();
            let pins = head.aligned_target_pins();
            let state = runner.state_mut();
            state.park_ability_continuation(PendingContinuation::new(Box::new(head), state));
            state.waiting_for = WaitingFor::DigChoice {
                player: P0,
                library_owner: P0,
                cards: vec![image, rest],
                keep_count: 1,
                up_to: true,
                selectable_cards: vec![image],
                kept_destination: Some(Zone::Battlefield),
                rest_destination: Some(Zone::Library),
                rest_split_top_count: None,
                rest_order: DigRestOrder::Preserve,
                source_id: Some(source),
                enter_tapped: false,
                enters_attacking: false,
            };
            let mut events = runner
                .act(GameAction::SelectCards {
                    cards: if keep { vec![image] } else { vec![] },
                })
                .unwrap()
                .events;
            if keep {
                // CR 614.1c + CR 614.12a: the actual Image replacement parks delivery.
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ));
                assert_eq!(runner.state().objects[&image].zone, Zone::Library);
                let batch = runner.state().active_batch_delivery().unwrap();
                assert!(
                    matches!(&batch.completion, Some(engine::types::game_state::BatchCompletion::RevealRestPile { publish_tracked_set: Some(ids), continuation_targets, .. }) if ids == &vec![image] && continuation_targets == &vec![image])
                );
                assert!(runner
                    .state()
                    .tracked_object_sets
                    .values()
                    .all(|ids| !ids.contains(&image)));
                let pending = runner
                    .state()
                    .resolution_stack
                    .iter()
                    .find_map(|frame| match frame {
                        engine::types::resolution::ResolutionFrame::AbilityContinuation(frame) => {
                            Some(&frame.pending)
                        }
                        _ => None,
                    })
                    .expect("the batch parks above the retained continuation");
                assert_eq!(pending.chain.targets, targets);
                assert_eq!(pending.chain.aligned_target_pins(), pins);
                assert_eq!(pending.chain.source_id, source);
                assert_eq!(pending.chain.controller, P0);
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement { index: 0 })
                        .unwrap()
                        .events,
                );
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::CopyTargetChoice { .. }
                ));
                events.extend(
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(copy_target)),
                        })
                        .unwrap()
                        .events,
                );
                assert_eq!(runner.state().objects[&image].zone, Zone::Battlefield);
                assert_eq!(runner.state().objects[&image].name, "Grizzly Bears");
                assert!(runner
                    .state()
                    .tracked_object_sets
                    .values()
                    .any(|ids| ids.contains(&image)));
            }
            // CR 601.2c: batch settlement never substitutes Image for announced P1.
            assert_eq!(
                runner.state().players[1].life,
                if own_target { 18 } else { 20 }
            );
            assert_eq!(
                runner.state().players[0].life,
                if !own_target && keep { 23 } else { 20 }
            );
            if own_target {
                assert!(events.iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. } if *source_id == source && *player == P1)));
            }
            assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
            assert!(runner.state().active_ability_continuation().is_none());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}
