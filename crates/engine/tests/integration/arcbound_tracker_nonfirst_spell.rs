//! Public-cast regressions for possessive nonfirst-spell qualification.

use engine::game::scenario::{CardBuilder, GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, GameState, ShardChoice, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const TRACKER: &str = "Menace\nModular 2 (This creature enters with two +1/+1 counters on it. When it dies, you may put its +1/+1 counters on target artifact creature.)\nWhenever you cast a spell other than your first spell each turn, put a +1/+1 counter on this creature.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const FLICKER: &str = "Exile target creature or enchantment, then return it to the battlefield under its owner's control.";
const ACT_OF_AGGRESSION: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";
const COUNTERSPELL: &str = "Counter target spell.";

fn tracker_card(card: &mut CardBuilder<'_>) -> ObjectId {
    card.as_artifact_creature()
        .with_subtypes(vec!["Dog"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Menace", "Modular"], TRACKER)
        .id()
}

fn battlefield_tracker(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let tracker = tracker_card(&mut scenario.add_creature(player, "Arcbound Tracker", 0, 0));
    // Pregame board state; public entry is exercised separately below.
    scenario.with_counter(tracker, CounterType::Plus1Plus1, 2);
    tracker
}

fn bolt(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Lightning Bolt", true, BOLT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id()
}

fn memnite(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_hand(player, "Memnite", 1, 1)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("")
        .id()
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_life(player, 50);
        scenario.with_library_top(player, &["Plains"; 8]);
        scenario.with_mana_pool(
            player,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); 20],
        );
    }
    scenario
}

fn counters(state: &GameState, tracker: ObjectId) -> u32 {
    state.objects[&tracker]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_complete(state: &GameState) {
    assert!(
        state.stack.is_empty(),
        "unresolved stack: {:?}",
        state.stack
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    if runner.state().priority_player != player {
        runner
            .act(GameAction::PassPriority)
            .expect("pass to caster");
    }
    assert_eq!(runner.state().waiting_for, WaitingFor::Priority { player });
}

fn tracker_triggers(state: &GameState, tracker: ObjectId) -> Vec<ObjectId> {
    state
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == tracker
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|entry| entry.id)
        .collect()
}

// CR 603.2 + CR 603.2c: exclude the first event, then trigger once for every
// later cast, regardless of spell type; CR 608.2c: put one counter on this source.
#[test]
fn tracker_first_second_third() {
    let mut scenario = scenario();
    let tracker = battlefield_tracker(&mut scenario, P0);
    let first = bolt(&mut scenario, P0);
    let second = memnite(&mut scenario, P0);
    let third = bolt(&mut scenario, P0);
    let mut runner = scenario.build();
    let baseline = counters(runner.state(), tracker);

    runner.cast(first).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_complete(runner.state());
    let after_first = counters(runner.state(), tracker);
    runner.cast(second).resolve();
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_complete(runner.state());
    let after_second = counters(runner.state(), tracker);
    runner.cast(third).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&third].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&tracker].zone, Zone::Battlefield);
    assert_eq!(runner.life(P1), 44);
    assert_complete(runner.state());
    let after_third = counters(runner.state(), tracker);
    let deltas = [
        after_first - baseline,
        after_second - after_first,
        after_third - after_second,
    ];
    println!("Tracker first/second/third counter deltas: {deltas:?}; stack empty, Priority");
    // Collect and check the positive reach guards before the first-cast refusal.
    assert_eq!(&deltas[1..], &[1, 1]);
    assert_eq!(deltas, [0, 1, 1]);
}

// CR 601.2i: completed prior casts count even before the source exists.
// CR 702.43a: Modular 2 must supply the real 0/0 creature's entry counters.
#[test]
fn tracker_counts_casts_before_entry() {
    let mut scenario = scenario();
    let tracker = tracker_card(&mut scenario.add_creature_to_hand(P0, "Arcbound Tracker", 0, 0));
    let later = bolt(&mut scenario, P0);
    let mut runner = scenario.build();

    runner.cast(tracker).resolve();
    assert_complete(runner.state());
    assert_eq!(runner.state().objects[&tracker].zone, Zone::Battlefield);
    let entry_counters = counters(runner.state(), tracker);
    println!("Tracker public Modular entry counters: {entry_counters}");
    assert_eq!(entry_counters, 2);
    runner.cast(later).target_player(P1).resolve();
    assert_complete(runner.state());
    assert_eq!(runner.state().objects[&later].zone, Zone::Graveyard);
    assert_eq!(runner.life(P1), 47);
    let delta = counters(runner.state(), tracker) - entry_counters;
    println!("Tracker post-entry cast counter delta: {delta}; stack empty, Priority");
    assert_eq!(delta, 1);
}

// CR 601.2i: neither resolving nor countering a completed cast erases history.
#[test]
fn tracker_counts_resolved_or_countered_casts_before_entry() {
    for counter_prior in [false, true] {
        let mut scenario = scenario();
        let earlier = bolt(&mut scenario, P0);
        let tracker =
            tracker_card(&mut scenario.add_creature_to_hand(P0, "Arcbound Tracker", 0, 0));
        let later = bolt(&mut scenario, P0);
        let counter = scenario
            .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            })
            .id();
        scenario.with_mana_pool(
            P1,
            vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]); 2],
        );
        let mut runner = scenario.build();
        if counter_prior {
            let mut cast = runner.cast(earlier).target_player(P1).commit();
            cast.act(GameAction::PassPriority).expect("offer response");
            assert_eq!(
                cast.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            );
            cast.cast(counter).target_object(earlier).resolve();
            cast.resolve();
            assert_eq!(runner.state().objects[&counter].zone, Zone::Graveyard);
            assert_eq!(runner.life(P1), 50);
        } else {
            runner.cast(earlier).target_player(P1).resolve();
            assert_eq!(runner.life(P1), 47);
        }
        assert_eq!(runner.state().objects[&earlier].zone, Zone::Graveyard);
        assert_complete(runner.state());
        runner.cast(tracker).resolve();
        assert_eq!(runner.state().objects[&tracker].zone, Zone::Battlefield);
        assert_eq!(counters(runner.state(), tracker), 2, "actual Modular entry");
        assert_complete(runner.state());
        runner.cast(later).target_player(P1).resolve();
        assert_eq!(runner.state().objects[&later].zone, Zone::Graveyard);
        assert_eq!(counters(runner.state(), tracker), 3);
        assert_eq!(runner.life(P1), if counter_prior { 47 } else { 44 });
        assert_complete(runner.state());
    }
}

// CR 603.2: "each turn" includes the opponent's turn. CR 500.2: pass through
// real turn boundaries rather than clearing the per-caster history in setup.
#[test]
fn tracker_turn_reset_and_offturn() {
    let mut scenario = scenario();
    let tracker = battlefield_tracker(&mut scenario, P0);
    let first = memnite(&mut scenario, P0);
    let second = bolt(&mut scenario, P0);
    let own_offturn = [bolt(&mut scenario, P0), bolt(&mut scenario, P0)];
    let opponents = [bolt(&mut scenario, P1), bolt(&mut scenario, P1)];
    let own_lands = [
        scenario.add_basic_land(P0, ManaColor::Red),
        scenario.add_basic_land(P0, ManaColor::Red),
    ];
    let opponent_lands = [
        scenario.add_basic_land(P1, ManaColor::Red),
        scenario.add_basic_land(P1, ManaColor::Red),
    ];
    let mut runner = scenario.build();
    let baseline = counters(runner.state(), tracker);
    runner.cast(first).resolve();
    assert_eq!(counters(runner.state(), tracker), baseline);
    assert_complete(runner.state());
    runner.cast(second).target_player(P1).resolve();
    assert_eq!(counters(runner.state(), tracker), baseline + 1);
    assert_complete(runner.state());
    let original_turn = runner.state().turn_number;
    for _ in 0..64 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("no attacks");
            }
            ref other => panic!("unexpected turn boundary: {other:?}"),
        }
    }
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().turn_number, original_turn + 1);
    assert_complete(runner.state());
    for land in opponent_lands {
        runner.activate(land, 0).resolve();
    }
    runner.cast(opponents[0]).target_player(P0).resolve();
    assert_eq!(counters(runner.state(), tracker), baseline + 1);
    assert_complete(runner.state());
    give_priority(&mut runner, P0);
    for land in own_lands {
        runner.activate(land, 0).resolve();
    }
    runner.cast(own_offturn[0]).target_player(P1).resolve();
    let after_first_offturn = counters(runner.state(), tracker);
    assert_complete(runner.state());
    runner.cast(opponents[1]).target_player(P0).resolve();
    assert_eq!(counters(runner.state(), tracker), after_first_offturn);
    assert_complete(runner.state());
    give_priority(&mut runner, P0);
    runner.cast(own_offturn[1]).target_player(P1).resolve();
    let after_second_offturn = counters(runner.state(), tracker);
    assert_eq!(
        after_second_offturn - after_first_offturn,
        1,
        "offturn reach"
    );
    assert_eq!(
        after_first_offturn,
        baseline + 1,
        "new turn's first cast is excluded"
    );
    assert_eq!(after_second_offturn, baseline + 2);
    assert_eq!(runner.life(P0), 44);
    assert_eq!(runner.life(P1), 41);
    assert_complete(runner.state());
}

// CR 109.5 + CR 603.2c: "you" is each source's controller, not its owner;
// independent sources each trigger exactly once on that controller's later cast.
#[test]
fn tracker_source_and_caster_scopes() {
    let mut scenario = scenario();
    let own = battlefield_tracker(&mut scenario, P0);
    let stolen = tracker_card(
        scenario
            .add_creature(P1, "Arcbound Tracker", 0, 0)
            .controlled_by(P0),
    );
    scenario.with_counter(stolen, CounterType::Plus1Plus1, 2);
    let opposing = battlefield_tracker(&mut scenario, P1);
    let first = bolt(&mut scenario, P0);
    let second = memnite(&mut scenario, P0);
    let opponents = [bolt(&mut scenario, P1), bolt(&mut scenario, P1)];
    let mut runner = scenario.build();
    let baseline = [own, stolen, opposing].map(|id| counters(runner.state(), id));
    assert_eq!(runner.state().objects[&stolen].owner, P1);
    assert_eq!(runner.state().objects[&stolen].controller, P0);
    runner.cast(first).target_player(P1).resolve();
    assert_eq!(
        [own, stolen, opposing].map(|id| counters(runner.state(), id)),
        baseline
    );
    assert_complete(runner.state());
    runner.cast(second).resolve();
    let after_own = [own, stolen, opposing].map(|id| counters(runner.state(), id));
    assert_eq!(after_own, [baseline[0] + 1, baseline[1] + 1, baseline[2]]);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_complete(runner.state());
    give_priority(&mut runner, P1);
    runner.cast(opponents[0]).target_player(P0).resolve();
    assert_eq!(
        [own, stolen, opposing].map(|id| counters(runner.state(), id)),
        after_own
    );
    assert_complete(runner.state());
    give_priority(&mut runner, P1);
    runner.cast(opponents[1]).target_player(P0).resolve();
    assert_eq!(
        [own, stolen, opposing].map(|id| counters(runner.state(), id)),
        [after_own[0], after_own[1], after_own[2] + 1],
    );
    assert_eq!(runner.life(P0), 44);
    assert_eq!(runner.life(P1), 47);
    assert_complete(runner.state());
}

// CR 113.7a: a pending trigger is independent of its source. CR 400.7: a
// blinked source is a new incarnation, not the old counter instruction's recipient.
#[test]
fn tracker_pending_trigger_does_not_mark_returned_source() {
    for blink in [false, true] {
        let mut scenario = scenario();
        let tracker = battlefield_tracker(&mut scenario, P0);
        let spare = battlefield_tracker(&mut scenario, P0);
        let first = memnite(&mut scenario, P0);
        let second = bolt(&mut scenario, P0);
        let flicker = scenario
            .add_spell_to_hand_from_oracle(P1, "Flicker of Fate", true, FLICKER)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .id();
        scenario.with_mana_pool(
            P1,
            vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        runner.cast(first).resolve();
        assert_complete(runner.state());
        let baseline = counters(runner.state(), tracker);
        let spare_baseline = counters(runner.state(), spare);
        let incarnation = runner.state().objects[&tracker].incarnation;
        let mut cast = runner.cast(second).target_player(P1).commit();
        let old_triggers = tracker_triggers(cast.state(), tracker);
        assert_eq!(old_triggers.len(), 1, "qualifying event queues one trigger");
        assert_eq!(tracker_triggers(cast.state(), spare).len(), 1);
        assert_eq!(
            counters(cast.state(), tracker),
            baseline,
            "not resolved yet"
        );
        if blink {
            cast.act(GameAction::PassPriority)
                .expect("offer blink response");
            assert_eq!(
                cast.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            );
            {
                let mut response = cast.cast(flicker).target_object(tracker).commit();
                assert_eq!(response.state().stack.back().expect("response").id, flicker);
                response.act(GameAction::PassPriority).expect("P1 passes");
                response
                    .act(GameAction::PassPriority)
                    .expect("P0 passes; blink resolves");
            }
            assert_eq!(cast.state().objects[&flicker].zone, Zone::Graveyard);
            assert_eq!(cast.state().objects[&tracker].zone, Zone::Battlefield);
            assert_ne!(cast.state().objects[&tracker].incarnation, incarnation);
            // CR 702.43a: these are actual replacement-entry counters, not
            // counters from the still-pending old trigger.
            assert_eq!(counters(cast.state(), tracker), 2);
            assert_eq!(tracker_triggers(cast.state(), tracker), old_triggers);
            assert_eq!(counters(cast.state(), spare), spare_baseline);
            assert!(matches!(
                cast.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
        cast.resolve();
        assert_eq!(
            counters(runner.state(), tracker),
            if blink { 2 } else { baseline + 1 },
        );
        assert_eq!(counters(runner.state(), spare), spare_baseline + 1);
        assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
        assert_eq!(runner.life(P1), 47);
        assert_complete(runner.state());
    }
}

// CR 603.3a: an earned trigger keeps its controller. CR 109.5: later events
// use the source's new controller; the self-counter still names the same object.
#[test]
fn tracker_trigger_keeps_controller_after_control_change() {
    let mut scenario = scenario();
    let tracker = battlefield_tracker(&mut scenario, P0);
    let first = memnite(&mut scenario, P0);
    let second = bolt(&mut scenario, P0);
    let old_controller_later = bolt(&mut scenario, P0);
    let new_controller_later = bolt(&mut scenario, P1);
    let takeover = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Aggression", true, ACT_OF_AGGRESSION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianRed, ManaCostShard::PhyrexianRed],
            generic: 3,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(first).resolve();
    assert_complete(runner.state());
    let baseline = counters(runner.state(), tracker);
    let incarnation = runner.state().objects[&tracker].incarnation;
    let mut cast = runner.cast(second).target_player(P1).commit();
    let old_triggers = tracker_triggers(cast.state(), tracker);
    assert_eq!(old_triggers.len(), 1);
    assert_eq!(counters(cast.state(), tracker), baseline);
    cast.act(GameAction::PassPriority)
        .expect("offer takeover response");
    assert_eq!(
        cast.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    // The fluent cast helper does not answer PhyrexianPayment; submit this
    // response's real target and payment choices through the same public seam.
    let card_id = cast.state().objects[&takeover].card_id;
    cast.act(GameAction::CastSpell {
        object_id: takeover,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("announce Act of Aggression");
    assert!(matches!(
        cast.state().waiting_for,
        WaitingFor::TargetSelection { player: P1, .. }
    ));
    cast.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(tracker)),
    })
    .expect("choose the opposing Tracker");
    let WaitingFor::PhyrexianPayment {
        player,
        spell_object,
        shards,
        ..
    } = &cast.state().waiting_for
    else {
        panic!("Act of Aggression must ask for its Phyrexian payment");
    };
    assert_eq!(*player, P1);
    assert_eq!(*spell_object, takeover);
    assert_eq!(shards.len(), 2);
    cast.act(GameAction::SubmitPhyrexianChoices {
        choices: vec![ShardChoice::PayMana, ShardChoice::PayMana],
    })
    .expect("pay both printed Phyrexian symbols with red mana");
    assert_eq!(
        cast.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    assert_eq!(cast.state().stack.back().expect("response").id, takeover);
    cast.act(GameAction::PassPriority).expect("P1 passes");
    cast.act(GameAction::PassPriority)
        .expect("P0 passes; takeover resolves");
    assert_eq!(cast.state().objects[&takeover].zone, Zone::Graveyard);
    assert_eq!(cast.state().objects[&tracker].owner, P0);
    assert_eq!(cast.state().objects[&tracker].controller, P1);
    assert_eq!(cast.state().objects[&tracker].incarnation, incarnation);
    assert_eq!(tracker_triggers(cast.state(), tracker), old_triggers);
    let pending = cast
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == old_triggers[0])
        .unwrap();
    assert_eq!(pending.controller, P0);
    assert_eq!(
        pending
            .ability()
            .expect("pending counter ability")
            .controller,
        P0
    );
    assert_eq!(counters(cast.state(), tracker), baseline);
    assert!(matches!(
        cast.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    cast.resolve();
    assert_eq!(counters(runner.state(), tracker), baseline + 1);
    assert_complete(runner.state());
    runner
        .cast(old_controller_later)
        .target_player(P1)
        .resolve();
    assert_eq!(counters(runner.state(), tracker), baseline + 1);
    assert_complete(runner.state());
    give_priority(&mut runner, P1);
    runner
        .cast(new_controller_later)
        .target_player(P0)
        .resolve();
    assert_eq!(counters(runner.state(), tracker), baseline + 2);
    assert_eq!(runner.life(P0), 47);
    assert_eq!(runner.life(P1), 44);
    assert_complete(runner.state());
}

// CR 601.2i + CR 603.2c: each qualifying cast earns its own trigger before
// either resolves; no exact-second or once-per-turn substitute is equivalent.
#[test]
fn tracker_qualifies_each_event_before_resolution() {
    let mut scenario = scenario();
    let tracker = battlefield_tracker(&mut scenario, P0);
    let first = memnite(&mut scenario, P0);
    let second = bolt(&mut scenario, P0);
    let third = bolt(&mut scenario, P0);
    let mut runner = scenario.build();
    runner.cast(first).resolve();
    assert_complete(runner.state());
    let baseline = counters(runner.state(), tracker);
    let mut cast = runner.cast(second).target_player(P1).commit();
    let second_triggers = tracker_triggers(cast.state(), tracker);
    assert_eq!(second_triggers.len(), 1);
    assert_eq!(counters(cast.state(), tracker), baseline);
    {
        let response = cast.cast(third).target_player(P1).commit();
        let triggers = tracker_triggers(response.state(), tracker);
        assert_eq!(triggers.len(), 2);
        assert!(triggers.contains(&second_triggers[0]));
        assert_eq!(counters(response.state(), tracker), baseline);
        assert!(matches!(
            response.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
    cast.resolve();
    assert_eq!(counters(runner.state(), tracker), baseline + 2);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&third].zone, Zone::Graveyard);
    assert_eq!(runner.life(P1), 44);
    assert_complete(runner.state());
}
