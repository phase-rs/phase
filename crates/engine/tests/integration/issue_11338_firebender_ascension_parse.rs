//! RED-first regression for #11338: Firebender Ascension's observer trigger
//! ("Whenever a creature you control attacking causes a triggered ability of
//! that creature to trigger") must fire from the card's verbatim Oracle text.
//!
//! Unlike `ability_triggered_observer.rs` (phase 1: hand-installs the D1 shape
//! because the parser cannot produce it), these tests add the Ascension from
//! verbatim Oracle and install NOTHING: they fail while the trigger condition
//! parses as mode `Unknown` and pass once the parser lowers it to
//! `TriggerMode::AbilityTriggered` (phase 2, this PR).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

// Verbatim Oracle text from `client/public/card-data.json` (#11338).
const FIREBENDER_ASCENSION: &str = "When this enchantment enters, create a 2/2 red Soldier creature token with firebending 1.\nWhenever a creature you control attacking causes a triggered ability of that creature to trigger, put a quest counter on this enchantment. Then if it has four or more quest counters on it, you may copy that ability. You may choose new targets for the copy.";
// Zuko fixture shared with `ability_triggered_observer.rs` (its firebending
// attack trigger is the observed ability).
const ZUKO: &str = "Firebending 1 (Whenever this creature attacks, add {R}. This mana lasts until end of combat.)\nWhenever you cast a noncreature spell, Zuko gains first strike until end of turn.\nWhenever Zuko deals combat damage to a player, put a +1/+1 counter on him.";
// Verbatim Oracle of Iroh, Dragon of the West (shared with
// `ability_triggered_observer.rs`): Mentor with no lesser-power attacker is
// removed during placement, so the observer fires with `stack_entry: None`.
const IROH: &str = "Haste\nMentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nAt the beginning of combat on your turn, each creature you control with a counter on it gains firebending 2 until end of turn. (Whenever it attacks, add {R}{R}. This mana lasts until end of combat.)";

fn quest_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Generic("quest".to_string()))
        .copied()
        .unwrap_or(0)
}

fn triggered_entries_of(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source)
        })
        .count()
}

/// Declare `attackers` against P1 from P0's precombat main phase.
fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "expected the declare-attackers prompt, got {:?}",
        runner.state().waiting_for
    );
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("attack declaration is accepted");
}

/// Answer every `OrderTriggers` prompt with the identity order.
fn order_identity(runner: &mut GameRunner) {
    for _ in 0..8 {
        let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
            return;
        };
        let order: Vec<usize> = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("trigger ordering is accepted");
    }
    panic!("OrderTriggers never cleared");
}

/// Add the Ascension and Zuko from verbatim Oracle; the parser must produce
/// the observer trigger condition (no hand-installed shape).
fn observer_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = scenario
        .add_enchantment_from_oracle(P0, "Firebender Ascension", FIREBENDER_ASCENSION)
        .id();
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let runner = scenario.build();
    (runner, ascension, zuko)
}

/// The resolution loop is done when the stack drained or the game ended
/// (without the game-over break a never-arriving prompt would deck a player
/// and mask the real failure as `GameOver`).
fn drained_or_over(runner: &GameRunner) -> bool {
    runner.state().stack.is_empty()
        || matches!(runner.state().waiting_for, WaitingFor::GameOver { .. })
}

fn assert_no_optional_choice(runner: &GameRunner) {
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the copy clause (four or more quest counters) must not prompt below four counters"
    );
}

/// Below four quest counters the observed attack trigger puts one quest
/// counter on the Ascension and the copy clause never prompts.
#[test]
fn firebender_attack_trigger_puts_quest_counter_from_verbatim_oracle() {
    let (mut runner, ascension, zuko) = observer_board();

    attack(&mut runner, &[zuko]);
    order_identity(&mut runner);

    // Reach guard: Zuko's firebending trigger fired (the attack pipeline ran),
    // so the quest assertion below discriminates the observer, not the attack.
    assert!(
        triggered_entries_of(runner.state(), zuko) > 0,
        "Zuko's firebending trigger must be on the stack"
    );

    // V-ORDER (R4/R10, CR 603.3b two-part placement): the observer entry
    // sits above — resolves before — the causing ability.
    let stack = &runner.state().stack;
    let zuko_pos = stack
        .iter()
        .position(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == zuko
            )
        })
        .expect("zuko entry positioned");
    let obs_pos = stack
        .iter()
        .position(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility {
                    source_id,
                    trigger_event: Some(GameEvent::AbilityTriggered { .. }),
                    ..
                } if *source_id == ascension
            )
        })
        .expect("observer entry positioned");
    assert!(
        zuko_pos < obs_pos,
        "the observer must sit above the causing ability (zuko {zuko_pos}, observer {obs_pos})"
    );

    for _ in 0..40 {
        assert_no_optional_choice(&runner);
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        quest_counters(runner.state(), ascension),
        1,
        "the Ascension must observe Zuko's attack trigger and put a quest counter on itself"
    );
}

/// Stage three quest counters, then attack: the fourth counter engages the
/// copy clause, which prompts. Declining ends the chain cleanly.
#[test]
fn firebender_decline_copy_at_four_counters_keeps_quest_counters() {
    let (mut runner, ascension, zuko) = observer_board();
    runner
        .state_mut()
        .objects
        .get_mut(&ascension)
        .expect("ascension exists")
        .counters
        .insert(CounterType::Generic("quest".to_string()), 3);

    attack(&mut runner, &[zuko]);
    order_identity(&mut runner);

    assert!(
        triggered_entries_of(runner.state(), zuko) > 0,
        "Zuko's firebending trigger must be on the stack"
    );

    for _ in 0..40 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the copy clause must prompt at four quest counters, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        quest_counters(runner.state(), ascension),
        4,
        "the observed trigger must have put the fourth quest counter on"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline is accepted");
    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain after declining, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        quest_counters(runner.state(), ascension),
        4,
        "declining the copy keeps all four quest counters"
    );
}

/// Accepting the copy puts a second copy of the observed trigger on the stack.
#[test]
fn firebender_accepted_copy_puts_observed_trigger_on_stack_again() {
    let (mut runner, ascension, zuko) = observer_board();
    runner
        .state_mut()
        .objects
        .get_mut(&ascension)
        .expect("ascension exists")
        .counters
        .insert(CounterType::Generic("quest".to_string()), 3);

    attack(&mut runner, &[zuko]);
    order_identity(&mut runner);

    for _ in 0..40 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the copy clause must prompt at four quest counters, got {:?}",
        runner.state().waiting_for
    );

    // The accept pushes the copy synchronously: no pass happens between the
    // two counts, so the original cannot resolve in between.
    let before = triggered_entries_of(runner.state(), zuko);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is accepted");
    assert_eq!(
        triggered_entries_of(runner.state(), zuko),
        before + 1,
        "accepting the copy must put a second Zuko trigger on the stack"
    );

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain after the copy resolves, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        quest_counters(runner.state(), ascension),
        4,
        "quest counters are untouched by the copy resolving"
    );
}

/// V-FIRE-MULTI: two attackers → each copy duplicates its own firing's
/// trigger (behavioral binding by entry id, not stack position). Two Zukos
/// have identical effects but distinct objects, forcing object-level
/// attribution — a `stack.last()`-style binding fails the per-source delta.
#[test]
fn firebender_two_attackers_each_copy_duplicates_its_own_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = scenario
        .add_enchantment_from_oracle(P0, "Firebender Ascension", FIREBENDER_ASCENSION)
        .id();
    let zuko_a = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let zuko_b = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    assert_ne!(zuko_a, zuko_b, "distinct attacker objects");
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&ascension)
        .expect("ascension exists")
        .counters
        .insert(CounterType::Generic("quest".to_string()), 3);

    attack(&mut runner, &[zuko_a, zuko_b]);
    order_identity(&mut runner);

    // Reach guards: both firebending triggers fired.
    assert!(
        triggered_entries_of(runner.state(), zuko_a) > 0,
        "zuko A firebending must be on the stack"
    );
    assert!(
        triggered_entries_of(runner.state(), zuko_b) > 0,
        "zuko B firebending must be on the stack"
    );

    // Per prompt: the parked frame's captured event names THIS firing's
    // observed entry (`current_trigger_event` is None while paused); the
    // copy must duplicate exactly that source.
    let mut seen_sources = Vec::new();
    for _ in 0..2 {
        for _ in 0..40 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                break;
            }
            if drained_or_over(&runner) {
                break;
            }
            runner.pass_both_players();
            order_identity(&mut runner);
        }
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "the copy clause must prompt, got {:?}",
            runner.state().waiting_for
        );
        let observed_id = match runner
            .state()
            .active_optional_effect_frame()
            .and_then(|frame| frame.trigger_event.clone())
        {
            Some(GameEvent::AbilityTriggered {
                stack_entry: Some(id),
                ..
            }) => id,
            other => panic!("prompt frame must carry the observed entry, got {other:?}"),
        };
        let expected_source = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == observed_id)
            .and_then(|entry| match &entry.kind {
                StackEntryKind::TriggeredAbility { source_id, .. } => Some(*source_id),
                StackEntryKind::Spell { .. }
                | StackEntryKind::ActivatedAbility { .. }
                | StackEntryKind::KeywordAction { .. }
                | StackEntryKind::CombatDamage { .. } => None,
            })
            .expect("observed entry names a triggered source");
        assert!(
            expected_source == zuko_a || expected_source == zuko_b,
            "observed source is one of the attackers"
        );
        seen_sources.push(expected_source);
        let other = if expected_source == zuko_a {
            zuko_b
        } else {
            zuko_a
        };
        let before = triggered_entries_of(runner.state(), expected_source);
        let other_before = triggered_entries_of(runner.state(), other);
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept is accepted");
        assert_eq!(
            triggered_entries_of(runner.state(), expected_source),
            before + 1,
            "the copy must duplicate this firing's trigger"
        );
        assert_eq!(
            triggered_entries_of(runner.state(), other),
            other_before,
            "the sibling firing is untouched by this copy"
        );
    }
    assert_eq!(seen_sources.len(), 2);
    assert_ne!(
        seen_sources[0], seen_sources[1],
        "the two prompts observe distinct firings"
    );

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        quest_counters(runner.state(), ascension),
        5,
        "both firings put their quest counter"
    );
}

/// V-NONE-NOOP: accepting with `stack_entry: None` (observed-but-removed,
/// CR 603.3d) is a clean no-op — `Ok`, no `MissingParam`, chain ends.
/// Mirrors V5's Iroh board: Mentor with no lesser-power attacker is removed
/// during placement, so the observer fires with `None`.
#[test]
fn firebender_accept_with_removed_observed_trigger_is_clean_noop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = scenario
        .add_enchantment_from_oracle(P0, "Firebender Ascension", FIREBENDER_ASCENSION)
        .id();
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&ascension)
        .expect("ascension exists")
        .counters
        .insert(CounterType::Generic("quest".to_string()), 3);

    attack(&mut runner, &[iroh]);
    order_identity(&mut runner);

    for _ in 0..40 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the copy clause must prompt at four quest counters, got {:?}",
        runner.state().waiting_for
    );
    // Preamble (paired guard against vacuity): the firing really carries
    // `None` — proves the None path was hit, not the Some path.
    match runner
        .state()
        .active_optional_effect_frame()
        .and_then(|frame| frame.trigger_event.clone())
    {
        Some(GameEvent::AbilityTriggered {
            stack_entry: None,
            source_id,
            ..
        }) => {
            assert_eq!(source_id, iroh, "the None firing observes iroh");
        }
        other => panic!("expected a None firing, got {other:?}"),
    }
    // Reach guard: stack empty (modulo the resolving entry) pre-accept.
    let resolving = runner
        .state()
        .resolving_stack_entry
        .as_ref()
        .map(|entry| entry.id);
    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| Some(entry.id) == resolving),
        "stack empty modulo the resolving entry"
    );
    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is accepted");
    // Gone (not swallowed-Absent): the no-op journals EffectResolved for
    // the copy effect. `resolve_top` discards resolution errors, so act()
    // succeeding alone cannot tell Gone from Absent — the journal entry can.
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind, source_id, .. }
                if matches!(kind, engine::types::ability::EffectKind::CopySpell)
                    && *source_id == ascension
        )),
        "the Gone no-op must journal the copy's EffectResolved, got {:?}",
        result.events
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| Some(entry.id) == resolving),
        "no new stack entries across the accept (Gone pushes nothing)"
    );

    for _ in 0..40 {
        if drained_or_over(&runner) {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must drain, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        quest_counters(runner.state(), ascension),
        4,
        "quest counters are untouched by the no-op copy"
    );
}
