//! Regressions for issue #6677: conditional counter overrides must retain the
//! object established by their antecedent instruction.
//!
//! The real Oracle text first targets a creature, then says "put two +1/+1
//! counters on it instead" when that creature is another Hero. The override's
//! bare pronoun must resolve to the original target, never Wakandan Royal Guard.
//! Emiel the Blessed covers the parallel trigger-event anaphor path.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WAKANDAN_ROYAL_GUARD_ORACLE: &str = "Vigilance\n\
    When this creature enters, put a +1/+1 counter on target creature. If that creature is another Hero, put two +1/+1 counters on it instead.";

const EMIEL_THE_BLESSED_ORACLE: &str = "{3}: Exile another target creature you control, then return it to the battlefield under its owner's control.\n\
    Whenever another creature you control enters, you may pay {G/W}. If you do, put a +1/+1 counter on it. If it's a Unicorn, put two +1/+1 counters on it instead. ({G/W} can be paid with either {G} or {W}.)";

fn p1p1_counters(state: &engine::types::game_state::GameState, object: ObjectId) -> u32 {
    state
        .objects
        .get(&object)
        .and_then(|card| card.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn resolve_guard_targeting_creature(target_is_hero: bool) -> (u32, u32, u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let selected = if target_is_hero {
        scenario
            .add_creature(P0, "Selected Hero", 2, 2)
            .with_subtypes(vec!["Hero"])
            .id()
    } else {
        scenario.add_creature(P0, "Selected Soldier", 2, 2).id()
    };
    let unrelated_hero = scenario
        .add_creature(P0, "Unselected Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .id();
    let guard = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Wakandan Royal Guard",
            2,
            2,
            WAKANDAN_ROYAL_GUARD_ORACLE,
        )
        .with_subtypes(vec!["Human", "Soldier", "Hero"])
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(guard).target_object(selected).resolve();

    (
        p1p1_counters(outcome.state(), selected),
        p1p1_counters(outcome.state(), guard),
        p1p1_counters(outcome.state(), unrelated_hero),
    )
}

/// CR 603.2 + CR 115.1d + CR 608.2c + CR 122.1: the ETB trigger targets the
/// selected Hero, and its matching instead-override puts two counters on that
/// same object. The zero-counter siblings prove the pronoun did not rebound to
/// either the resolving guard or another legal Hero.
#[test]
fn wakandan_royal_guard_doubles_counters_on_the_selected_hero() {
    let (selected, guard, unrelated_hero) = resolve_guard_targeting_creature(true);

    assert_eq!(
        selected, 2,
        "the selected Hero must receive two +1/+1 counters"
    );
    assert_eq!(
        guard, 0,
        "Wakandan Royal Guard must not receive the counters"
    );
    assert_eq!(
        unrelated_hero, 0,
        "an unselected Hero must not receive the counters"
    );
}

/// CR 603.2 + CR 115.1d + CR 608.2c + CR 122.1: when the chosen creature is
/// not a Hero, the override does not apply and the printed base instruction
/// still places exactly one counter on that chosen object.
#[test]
fn wakandan_royal_guard_keeps_one_counter_on_a_nonhero_target() {
    let (selected, guard, unrelated_hero) = resolve_guard_targeting_creature(false);

    assert_eq!(
        selected, 1,
        "the non-Hero target must receive the base instruction's one counter"
    );
    assert_eq!(
        guard, 0,
        "Wakandan Royal Guard must not receive the counter"
    );
    assert_eq!(
        unrelated_hero, 0,
        "an unrelated Hero must not satisfy the selected-target condition"
    );
}

/// CR 603.2 + CR 608.2c + CR 122.1: Emiel's first counter instruction and
/// its Unicorn override both refer to the entering creature, represented by
/// `TriggeringSource`. The unrelated Unicorn proves the event reference is
/// preserved rather than widened to a battlefield filter.
#[test]
fn emiel_the_blessed_doubles_counters_on_the_entering_unicorn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emiel = scenario
        .add_creature_from_oracle(P0, "Emiel the Blessed", 4, 4, EMIEL_THE_BLESSED_ORACLE)
        .with_subtypes(vec!["Angel"])
        .id();
    let unrelated_unicorn = scenario
        .add_creature(P0, "Unrelated Unicorn", 2, 2)
        .with_subtypes(vec!["Unicorn"])
        .id();
    let entering_unicorn = scenario
        .add_creature_to_hand(P0, "Entering Unicorn", 2, 2)
        .with_subtypes(vec!["Unicorn"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, emiel, false, vec![])],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(entering_unicorn).accept_optional().resolve();

    assert!(
        outcome.state().players[P0.0 as usize].mana_pool.is_empty(),
        "reach-guard: accepting Emiel's optional payment must consume the supplied green mana"
    );
    assert_eq!(
        p1p1_counters(outcome.state(), entering_unicorn),
        2,
        "the entering Unicorn must receive Emiel's two +1/+1 counters"
    );
    assert_eq!(
        p1p1_counters(outcome.state(), emiel),
        0,
        "Emiel must not receive the entering creature's counters"
    );
    assert_eq!(
        p1p1_counters(outcome.state(), unrelated_unicorn),
        0,
        "an unrelated Unicorn must not satisfy the trigger-event anaphor"
    );
}

/// CR 603.2 + CR 608.2c + CR 122.1: accepting Emiel's payment still performs
/// the printed one-counter continuation when the entering creature does not
/// satisfy the later Unicorn instead-condition.
#[test]
fn emiel_the_blessed_keeps_one_counter_on_a_nonunicorn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emiel = scenario
        .add_creature_from_oracle(P0, "Emiel the Blessed", 4, 4, EMIEL_THE_BLESSED_ORACLE)
        .with_subtypes(vec!["Angel"])
        .id();
    let entering_cat = scenario
        .add_creature_to_hand(P0, "Entering Cat", 2, 2)
        .with_subtypes(vec!["Cat"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, emiel, false, vec![])],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(entering_cat).accept_optional().resolve();

    assert!(
        outcome.state().players[P0.0 as usize].mana_pool.is_empty(),
        "reach-guard: accepting Emiel's optional payment must consume the supplied green mana"
    );
    assert_eq!(
        p1p1_counters(outcome.state(), entering_cat),
        1,
        "the entering non-Unicorn must receive the paid continuation's one counter"
    );
    assert_eq!(
        p1p1_counters(outcome.state(), emiel),
        0,
        "Emiel must not receive the entering creature's counter"
    );
}

/// Mesa Unicorn's printed Oracle text: a 2/2 Unicorn for {1}{W}.
const MESA_UNICORN_ORACLE: &str =
    "Lifelink (Damage dealt by this creature also causes you to gain that much life.)";

/// A land-paid Emiel resolution: the cast's outcome, the events of the
/// resolution alone (after the spell was committed to the stack), and the
/// entering creature, Emiel and the Forest.
struct LandPaidEmiel {
    outcome: engine::game::scenario::CastOutcome,
    resolution_events: Vec<engine::types::events::GameEvent>,
    entering: ObjectId,
    emiel: ObjectId,
    forest: ObjectId,
}

/// Cast `entering` with Emiel the Blessed and one untapped Forest on the
/// battlefield, its printed cost (Mesa Unicorn {1}{W}, Grizzly Bears {1}{G})
/// paid exactly from floating mana, then accept Emiel's "you may pay {G/W}".
/// The cast is committed first and checked to have left the Forest untapped and
/// nothing floating, so Emiel's payment can only tap the Forest while the
/// trigger resolves (CR 605.3a), never while the creature spell is cast.
fn cast_under_emiel_paying_with_a_forest(entering_is_unicorn: bool) -> Option<LandPaidEmiel> {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::zones::Zone;

    let db = crate::support::shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emiel = scenario.add_real_card(P0, "Emiel the Blessed", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let entering = if entering_is_unicorn {
        scenario
            .add_creature_to_hand_from_oracle(P0, "Mesa Unicorn", 2, 2, MESA_UNICORN_ORACLE)
            .with_subtypes(vec!["Unicorn"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::White],
                generic: 1,
            })
            .id()
    } else {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db)
    };
    let colored = if entering_is_unicorn {
        ManaType::White
    } else {
        ManaType::Green
    };
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(colored, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let commit = runner.cast(entering).accept_optional().commit();
    // Reach guard: casting the creature paid its printed cost from the pool alone.
    assert_eq!(
        commit.state().stack.len(),
        1,
        "the creature spell is on the stack"
    );
    assert!(
        !commit.state().objects[&forest].tapped,
        "casting the creature must not tap the Forest"
    );
    assert!(
        commit.state().players[P0.0 as usize].mana_pool.is_empty(),
        "nothing may float into Emiel's payment"
    );
    let committed = commit.events().len();
    let outcome = commit.resolve();
    let resolution_events = outcome.events()[committed..].to_vec();
    Some(LandPaidEmiel {
        outcome,
        resolution_events,
        entering,
        emiel,
        forest,
    })
}

/// CR 605.3a + CR 608.2c: Emiel's {G/W} is paid by tapping a Forest while the
/// trigger resolves. That tap is a mana payment, not a referent: "If it's a
/// Unicorn" still names the creature that entered, so Mesa Unicorn gets two
/// +1/+1 counters.
#[test]
fn emiel_the_blessed_doubles_counters_on_mesa_unicorn_when_a_land_pays() {
    let Some(paid) = cast_under_emiel_paying_with_a_forest(true) else {
        eprintln!("skipping: card database absent");
        return;
    };
    assert_emiel_payment_tapped_the_forest(&paid);
    let state = paid.outcome.state();
    assert_eq!(
        p1p1_counters(state, paid.entering),
        2,
        "Mesa Unicorn (a Unicorn) must receive Emiel's two +1/+1 counters"
    );
    assert_eq!(p1p1_counters(state, paid.emiel), 0);
    assert_eq!(p1p1_counters(state, paid.forest), 0);
}

/// Control for the test above: the same land-paid resolution on a non-Unicorn
/// keeps the one-counter continuation.
#[test]
fn emiel_the_blessed_keeps_one_counter_on_a_nonunicorn_when_a_land_pays() {
    let Some(paid) = cast_under_emiel_paying_with_a_forest(false) else {
        eprintln!("skipping: card database absent");
        return;
    };
    assert_emiel_payment_tapped_the_forest(&paid);
    let state = paid.outcome.state();
    assert_eq!(
        p1p1_counters(state, paid.entering),
        1,
        "Grizzly Bears must receive exactly the paid continuation's one counter"
    );
    assert_eq!(p1p1_counters(state, paid.emiel), 0);
    assert_eq!(p1p1_counters(state, paid.forest), 0);
}

/// Reach guard: the payment happened by tapping the Forest during resolution,
/// with nothing left floating, and the resolution completed.
fn assert_emiel_payment_tapped_the_forest(paid: &LandPaidEmiel) {
    let state = paid.outcome.state();
    assert!(
        state.objects[&paid.forest].tapped,
        "Emiel's {{G/W}} must be paid by tapping the Forest"
    );
    assert!(
        paid.resolution_events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::PermanentTapped { object_id, .. }
                if *object_id == paid.forest
        )),
        "the Forest must be tapped while Emiel's trigger resolves"
    );
    assert!(state.players[P0.0 as usize].mana_pool.is_empty());
    assert!(state.stack.is_empty(), "Emiel's trigger must have resolved");
    assert!(
        matches!(
            state.waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "resolution must end at priority, got {:?}",
        state.waiting_for
    );
}
