//! Issue #2358 regression: "leaves the battlefield, put those counters on X"
//! triggers must read the counters the creature had AS IT LEFT, from
//! last-known information (LKI), not from the post-move object whose counters
//! have already ceased to exist.
//!
//! Two cards exercise the same class through two different effect mechanisms:
//!
//!   * The Ozolith — "Whenever a creature you control leaves the battlefield,
//!     if it had counters on it, put those counters on The Ozolith."
//!     Mechanism: `MoveCounters { source: TriggeringSource, mode: Put }`. The
//!     transfer reads the source object's counter map; once the creature has
//!     left the battlefield that map is empty (CR 122.2), so the read must fall
//!     back to LKI.
//!
//!   * Reyhan, Last of the Abzan — "Whenever a creature you control dies, if it
//!     had one or more +1/+1 counters on it, you may put that many +1/+1
//!     counters on target creature." Mechanism: `PutCounter { count:
//!     EventContextAmount }`. The count is the number of +1/+1 counters the
//!     creature had, captured into the trigger event context at the moment it
//!     died.
//!
//! Root cause (CR 603.10 / CR 608.2h): counters cease to exist when a permanent
//! changes zones (CR 122.2). A "leaves the battlefield … the counters it had"
//! trigger is a look-back ability — it must use the creature's last-known
//! information, captured the instant before the zone change, not the live
//! (now-empty) object.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 122.2: If a permanent leaves the battlefield, all counters on it
//!     cease to exist.
//!   - CR 603.10: Some triggered abilities are "look back in time" — they
//!     trigger based on the game state immediately prior to an event.
//!   - CR 608.2h: If an effect of a triggered ability refers to information
//!     about the triggering object, it uses last-known information.

use super::rules::{GameScenario, Phase, WaitingFor, Zone, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;

/// Whirlpool Drake: a dies trigger whose "draw that many" is produced by a
/// preceding effect in the same resolution. The `EventContextAmount` look-back
/// fallback (CR 608.2h) must sit LAST in the cascade so it does not hijack this
/// count when the dying creature happens to carry +1/+1 counters.
const WHIRLPOOL_DRAKE_ORACLE: &str =
    "When this creature dies, shuffle the cards from your hand into your library, then draw that many cards.";

const OZOLITH_ORACLE: &str = "Whenever a creature you control leaves the battlefield, if it had counters on it, put those counters on The Ozolith.\nAt the beginning of combat on your turn, if The Ozolith has counters on it, you may move all counters from The Ozolith onto target creature.";

const REYHAN_ORACLE: &str = "Whenever a creature you control dies, if it had one or more +1/+1 counters on it, you may put that many +1/+1 counters on target creature.\nWhenever a creature you control is put into the command zone from the battlefield, if it had one or more +1/+1 counters on it, you may put that many +1/+1 counters on target creature.";

/// Count counters of a given type on an object.
fn counters(runner: &super::rules::GameRunner, id: ObjectId, ct: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(ct).copied())
        .unwrap_or(0)
}

/// The Ozolith collects the +1/+1 counters from a creature that leaves the
/// battlefield. The dying creature carried three +1/+1 counters; after it dies,
/// The Ozolith must hold exactly three +1/+1 counters (read from LKI, not from
/// the post-move object whose counters have ceased to exist).
#[test]
fn ozolith_collects_counters_from_leaving_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .id();

    let dying = scenario
        .add_creature_from_oracle(P0, "Counter Bearer", 2, 2, "")
        .with_plus_counters(3)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert_eq!(
        counters(&runner, dying, &CounterType::Plus1Plus1),
        3,
        "precondition: the creature carries 3 +1/+1 counters"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        0,
        "precondition: The Ozolith starts with no counters"
    );

    // The creature leaves the battlefield (dies). This is the look-back event
    // (CR 603.10): the trigger must read the counters it had as it left.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), dying, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    // The Ozolith's trigger places the counters on itself (target: SelfRef),
    // so no target selection is required — drive priority until it resolves.
    let mut guard = 0;
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        guard += 1;
        assert!(guard < 30, "trigger resolution did not terminate");
        if counters(&runner, ozolith, &CounterType::Plus1Plus1) > 0
            || runner.act(GameAction::PassPriority).is_err()
        {
            break;
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&dying).unwrap().zone,
        Zone::Graveyard,
        "control: the creature is now in the graveyard"
    );
    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        3,
        "The Ozolith must collect the 3 +1/+1 counters the creature had as it \
         left the battlefield (CR 122.2 + CR 608.2h LKI), not 0"
    );
}

/// Discrimination guard: a creature with NO counters that leaves the
/// battlefield must NOT give The Ozolith any counters — the `HadCounters`
/// intervening-if (CR 603.4) gates the trigger out, and the look-back fallback
/// must not invent counters. Proves the LKI read is faithful, not blanket.
#[test]
fn ozolith_ignores_counterless_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ozolith = scenario
        .add_creature_from_oracle(P0, "The Ozolith", 0, 0, OZOLITH_ORACLE)
        .as_artifact()
        .id();

    let dying = scenario
        .add_creature_from_oracle(P0, "Plain Creature", 2, 2, "")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), dying, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    let mut guard = 0;
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        guard += 1;
        assert!(guard < 30, "did not terminate");
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        counters(&runner, ozolith, &CounterType::Plus1Plus1),
        0,
        "a counterless departure must not give The Ozolith any counters (CR 603.4)"
    );
}

/// Sibling of the same class: Reyhan reads the *count* of +1/+1 counters the
/// dying creature had via `EventContextAmount`, then lets you put that many on a
/// target creature. The count must come from LKI captured at death.
#[test]
fn reyhan_moves_counter_count_from_dying_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Reyhan, Last of the Abzan", 4, 3, REYHAN_ORACLE)
        .as_legendary();

    let dying = scenario
        .add_creature_from_oracle(P0, "Counter Bearer", 1, 1, "")
        .with_plus_counters(4)
        .id();

    let receiver = scenario.add_creature(P0, "Receiver", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), dying, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);

    // Reyhan's trigger is optional and targets a creature: accept and aim at
    // the receiver. Drive the target-selection / priority windows.
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "trigger resolution did not terminate");
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(receiver)),
                    })
                    .expect("ChooseTarget should succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional trigger");
            }
            WaitingFor::Priority { .. } => {
                if counters(&runner, receiver, &CounterType::Plus1Plus1) > 0
                    || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        4,
        "Reyhan must put 4 +1/+1 counters (= the count the dying creature had) \
         on the receiver, read from LKI at death (CR 608.2h)"
    );
}

/// Count the cards in a player's hand.
fn hand_size(runner: &super::rules::GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand && o.controller == player)
        .count()
}

/// Regression guard for the `EventContextAmount` cascade ordering (issue #2358
/// adversarial review). Whirlpool Drake's "When this creature dies, shuffle the
/// cards from your hand into your library, then draw that many cards" resolves
/// "that many" to the number of cards SHUFFLED (the preceding effect's count via
/// `last_effect_count`), NOT the number of +1/+1 counters the Drake carried. The
/// LKI counter-count fallback must lose to `last_effect_count`/`last_effect_amount`
/// in the resolution cascade; otherwise a Drake dying with N +1/+1 counters would
/// draw N cards instead of the size of the shuffled hand.
#[test]
fn dies_trigger_draw_uses_shuffled_count_not_plus_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two cards in hand; library deep enough to satisfy either a correct (2) or
    // a buggy (3) draw, so the count — not a deck-out — is what the test pins.
    scenario.with_cards_in_hand(P0, &["Forest", "Island"]);
    scenario.with_library_top(P0, &["Mountain", "Plains", "Swamp", "Forest", "Island"]);

    let drake = scenario
        .add_creature_from_oracle(P0, "Whirlpool Drake", 2, 2, WHIRLPOOL_DRAKE_ORACLE)
        .with_plus_counters(3)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert_eq!(hand_size(&runner, P0), 2, "precondition: 2 cards in hand");

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), drake, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    let mut guard = 0;
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        guard += 1;
        assert!(guard < 30, "trigger resolution did not terminate");
        if hand_size(&runner, P0) != 0 && runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    runner.advance_until_stack_empty();

    // Shuffle moved both hand cards to the library (hand → 0), then "draw that
    // many" draws exactly 2 (the shuffled count), restoring the hand to 2 — not
    // 3 (the Drake's +1/+1 counter count).
    assert_eq!(
        hand_size(&runner, P0),
        2,
        "draw count must equal the 2 shuffled cards, not the Drake's 3 +1/+1 counters"
    );
}

const MINUS_COUNTER_REYHAN_ORACLE: &str = "Whenever a creature you control dies, if it had one or more -1/-1 counters on it, you may put that many -1/-1 counters on target creature.";

/// The LKI counter look-back must use the kind named by the resolving effect, not
/// a hardcoded +1/+1 (issue #2358 Gemini [HIGH] / adversarial review). A
/// Reyhan-class trigger that moves -1/-1 counters reads the -1/-1 count the dying
/// creature had. With the old hardcoded `Plus1Plus1`, the look-back would find no
/// +1/+1 counters and place 0 — this test fails on that path and passes once the
/// kind is parameterized from the `PutCounter` effect.
#[test]
fn dies_trigger_put_counter_respects_effect_counter_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Abzan Falconer", 4, 3, MINUS_COUNTER_REYHAN_ORACLE)
        .as_legendary();

    // 5/5 base with two -1/-1 counters → 3/3 on the battlefield (survives SBAs),
    // so the look-back has two -1/-1 counters to read from LKI at death.
    let dying = scenario
        .add_creature_from_oracle(P0, "Counter Bearer", 5, 5, "")
        .with_minus_counters(2)
        .id();

    let receiver = scenario.add_creature(P0, "Receiver", 4, 4).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), dying, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);

    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "trigger resolution did not terminate");
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(receiver)),
                    })
                    .expect("ChooseTarget should succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional trigger");
            }
            WaitingFor::Priority { .. } => {
                if counters(&runner, receiver, &CounterType::Minus1Minus1) > 0
                    || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        counters(&runner, receiver, &CounterType::Minus1Minus1),
        2,
        "the look-back must count the 2 -1/-1 counters named by the effect, not \
         hardcoded +1/+1 (which would place 0)"
    );
}
