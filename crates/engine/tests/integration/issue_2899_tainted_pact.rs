//! Regression for issue #2899 — Tainted Pact repeat-until loop and same-name
//! unless gate on the optional put-to-hand rider.
//!
//! https://github.com/phase-rs/phase/issues/2899
//!
//! Also covers the two outcomes of a Tainted Pact whose library runs out:
//!
//! - Issue #8798: with no card exiled, "you may put that card into your hand"
//!   has no referent. CR 608.2d forbids offering it, and CR 608.2c + CR 609.3
//!   forbid its `ParentTarget` from falling back to Tainted Pact itself.
//! - CR 104.4b + CR 732.4: once nothing is left to exile, every iteration is
//!   a mandatory no-op that can never meet either stop condition, so the game
//!   is a draw. A repeat whose stalled iteration offered an optional action is
//!   not a draw (the CR 104.4b carve-out) and just ends, as does one whose
//!   stalled iteration moved a card the progress witness does not count.
//! - CR 104.1: that draw stands even when a later prompt in the same action (a
//!   trigger-ordering or replacement-order choice) overwrites the wait.
//!
//! Fishing Gear (an `ExileTop` parent) and Jace, the Living Guildpact (a `Dig`
//! parent) cover the #8798 class outside Tainted Pact.

use std::sync::mpsc;
use std::time::Duration;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::attach::attach_to;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_library_position;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, EffectKind, LibraryPosition,
    QuantityExpr, QuantityRef, RepeatContinuation, ResolvedAbility, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::PlayerId;

use super::rules::run_combat;

const TAINTED_PACT_ORACLE: &str = "Exile the top card of your library. You may put that card into your hand unless it has the same name as another card exiled this way. Repeat this process until you put a card into your hand or you exile two cards with the same name, whichever comes first.";
const HAUNT_OF_HIGHTOWER_ORACLE: &str = "Flying, lifelink\nWhenever The Haunt of Hightower attacks, defending player discards a card.\nWhenever a card is put into an opponent's graveyard from anywhere, put a +1/+1 counter on The Haunt of Hightower.";
const BLOODCHIEF_ASCENSION_ORACLE: &str = "At the beginning of each end step, if an opponent lost 2 or more life this turn, you may put a quest counter on this enchantment. (Damage causes loss of life.)\nWhenever a card is put into an opponent's graveyard from anywhere, if this enchantment has three or more quest counters on it, you may have that player lose 2 life. If you do, you gain 2 life.";
const LEYLINE_OF_THE_VOID_ORACLE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";
const REST_IN_PEACE_ORACLE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const FISHING_GEAR_ORACLE: &str = "Whenever equipped creature deals combat damage to a player, exile the top card of that player's library. If it's a permanent card, you may put it onto the battlefield under your control. If you don't, create a 1/1 blue Fish creature token.\nEquip {2}";
const JACE_THE_LIVING_GUILDPACT_ORACLE: &str = "[+1]: Look at the top two cards of your library. Put one of them into your graveyard.\n[−3]: Return another target nonland permanent to its owner's hand.\n[−8]: Each player shuffles their hand and graveyard into their library. You draw seven cards.";

fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let mut events = Vec::new();
    move_to_library_position(runner.state_mut(), id, true, &mut events);
    assert_eq!(
        runner.state().players[owner.0 as usize].library[0],
        id,
        "precondition: card must be library top"
    );
}

fn resolve_tainted_pact(runner: &mut GameRunner, source: ObjectId, optional_accepts: &[bool]) {
    let def = parse_effect_chain(TAINTED_PACT_ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    for &accept in optional_accepts {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "expected optional put prompt, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("optional put decision");
    }
}

#[test]
fn tainted_pact_repeats_until_controller_puts_a_card_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let third = scenario
        .add_spell_to_library_top(P0, "Third Card", true)
        .id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second Card", true)
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First Card", true)
        .id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, first);

    resolve_tainted_pact(&mut runner, ObjectId(900), &[false, false, true]);

    assert_eq!(
        runner.state().objects.get(&third).unwrap().zone,
        Zone::Hand,
        "accepting the third optional put must move the top card into hand"
    );
    assert_eq!(
        runner.state().objects.get(&first).unwrap().zone,
        Zone::Exile,
        "declined iteration must leave the card exiled"
    );
    assert_eq!(
        runner.state().objects.get(&second).unwrap().zone,
        Zone::Exile,
        "declined iteration must leave the card exiled"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "loop must finish after a card is put into hand, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn tainted_pact_stops_when_two_exiled_cards_share_a_name() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolt_a = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let island = scenario.add_spell_to_library_top(P0, "Island", true).id();
    let bolt_b = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    // Exile order: bolt_b, island, bolt_a.
    put_library_top(&mut runner, bolt_b);

    resolve_tainted_pact(&mut runner, ObjectId(900), &[false, false]);

    assert_eq!(
        runner.state().objects.get(&bolt_b).unwrap().zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects.get(&island).unwrap().zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects.get(&bolt_a).unwrap().zone,
        Zone::Exile
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "unless gate must block the third optional put when names duplicate, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "duplicate-name stop must end the loop, got {:?}",
        runner.state().waiting_for
    );
}

/// `{1}{B}` worth of floating mana, so the cast is pool-funded and never
/// surfaces a `ManaPayment` window (CR 601.2g).
fn tainted_pact_mana() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]
}

fn tainted_pact_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    }
}

fn chain_contains_unimplemented(def: &AbilityDefinition) -> bool {
    if matches!(*def.effect, Effect::Unimplemented { .. }) {
        return true;
    }
    [def.sub_ability.as_deref(), def.else_ability.as_deref()]
        .into_iter()
        .flatten()
        .any(chain_contains_unimplemented)
}

/// Positive reach-guard shared by the termination regressions: the card really
/// parses to the loop variant whose missing termination guarantee is under
/// test, with no `Effect::Unimplemented` anywhere in the chain. Without this a
/// "the loop ended" assertion would pass vacuously on a card that never parsed
/// into a loop at all.
fn assert_tainted_pact_parses_to_until_stop_conditions() {
    let def = parse_effect_chain(TAINTED_PACT_ORACLE, AbilityKind::Spell);
    assert!(
        !chain_contains_unimplemented(&def),
        "reach-guard: Tainted Pact must parse with no Effect::Unimplemented"
    );
    assert!(
        matches!(
            def.repeat_until,
            Some(RepeatContinuation::UntilStopConditions { .. })
        ),
        "reach-guard: Tainted Pact must parse to the UntilStopConditions repeat, got {:?}",
        def.repeat_until
    );
}

/// Positive reach-guard for the two post-draw trigger rows (card-test
/// anti-pattern 6): `source` really carries the parsed "whenever a card is put
/// into an opponent's graveyard from anywhere" trigger (CR 603.2) that those
/// rows assume, its execute chain is real (no `Effect::Unimplemented` anywhere
/// in it), and `controller` controls it.
///
/// Without this the negative assertions downstream pass for the wrong reason on
/// a board where the oracle line silently stopped producing a trigger: there is
/// then nothing for CR 603.3b to order, nothing to put on the stack, and
/// nothing that could have placed a counter. The three pre-existing guards on
/// those rows all prove the *Pact* side only.
///
/// Returns the number of matching triggers on `source`, so a caller can pin
/// that CR 603.3b had two of them to ORDER rather than one to place unordered.
#[must_use]
fn assert_parses_to_opponent_graveyard_trigger(
    state: &GameState,
    source: ObjectId,
    controller: PlayerId,
) -> usize {
    let object = state.objects.get(&source).expect("guarded source object");
    assert_eq!(
        object.controller, controller,
        "reach-guard: {} must be controlled by the player whose triggers are \
         under test (CR 603.3b orders one player's triggers at a time)",
        object.name
    );
    let matching: Vec<_> = object
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| entry.definition())
        .filter(|def| {
            matches!(def.mode, TriggerMode::ChangesZone)
                && def.destination == Some(Zone::Graveyard)
                && matches!(
                    def.valid_card,
                    Some(TargetFilter::Typed(TypedFilter {
                        controller: Some(ControllerRef::Opponent),
                        ..
                    }))
                )
        })
        .collect();
    assert!(
        !matching.is_empty(),
        "reach-guard: {}'s \"whenever a card is put into an opponent's \
         graveyard from anywhere\" line must parse to an opponent-scoped \
         ChangesZone-to-graveyard trigger, got modes {:?}",
        object.name,
        object
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| &entry.definition().mode)
            .collect::<Vec<_>>()
    );
    for def in &matching {
        let execute = def.execute.as_deref().unwrap_or_else(|| {
            panic!(
                "reach-guard: {}'s graveyard trigger must carry an execute chain",
                object.name
            )
        });
        assert!(
            !chain_contains_unimplemented(execute),
            "reach-guard: {}'s graveyard trigger must parse with no \
             Effect::Unimplemented, got {execute:?}",
            object.name
        );
    }
    matching.len()
}

/// Positive reach-guard: the repeat's producer actually ran, so "the loop
/// ended" is not the trivially-true statement that it never started.
fn assert_exile_top_resolved(events: &[GameEvent]) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ExileTop,
                ..
            }
        )),
        "reach-guard: the repeat body must have resolved at least one ExileTop"
    );
}

/// Number of draw (`GameOver { winner: None }`) events in `events`.
fn draw_event_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::GameOver { winner: None }))
        .count()
}

/// CR 104.4b + CR 732.4 + CR 608.2d (issue #8798): Tainted Pact cast with an
/// empty library is a draw.
///
/// "Exile the top card of your library" exiles nothing (CR 609.3), so "that
/// card" has no referent: the "you may put that card into your hand" option is
/// impossible and never offered (CR 608.2d), and no printed stop condition can
/// ever become true. Every iteration is the same mandatory no-op with no way to
/// stop, so the game is a draw, and Tainted Pact is put into its owner's
/// graveyard as it finishes resolving (CR 608.2n).
///
/// The cast ACCEPTS every optional prompt, so it discriminates both halves:
/// - revert the #8798 hunks (the `ExileTop` missing-referent stamp plus the
///   `ChangeZone` feasibility arm / no-op guard) and the phantom prompt is
///   offered and accepted, the `ParentTarget` falls back to the source, Tainted
///   Pact goes to its owner's hand, and the paused iteration ends without a
///   draw — the GameOver and graveyard assertions both flip;
/// - revert the `MandatoryLoopDraw` verdict to a plain stop and the final
///   state is `Priority`.
#[test]
fn tainted_pact_empty_library_is_a_mandatory_loop_draw() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());

    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: this regression is about a starved producer"
    );

    let outcome = runner.cast(pact).accept_optional().resolve();

    assert_exile_top_resolved(outcome.events());
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "a mandatory loop with no way to stop is a draw (CR 104.4b), got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        draw_event_count(outcome.events()),
        1,
        "the draw must be announced exactly once"
    );
    outcome.assert_zone(&[pact], Zone::Graveyard);
    assert!(
        outcome.state().active_repeat_until().is_none(),
        "the repeat-until frame must retire, not stay parked"
    );
}

/// CR 104.4b + CR 732.4: a repeat that makes progress and THEN stalls is a
/// draw once only mandatory actions remain.
///
/// Iteration 1 exiles the only card and offers the put (declined, the harness
/// default), so it pauses and resumes through `drain_active_repeat_until`.
/// That resumed iteration re-enters `resolve_ability_chain`, whose iteration 2
/// finds the library empty, offers nothing, and draws. Its `GameOver` event is
/// emitted inside the resumed iteration, so the event-count assertion also pins
/// that the drain forwards resumed-iteration events to the action result.
///
/// It does NOT discriminate a baseline hoisted above the `loop` in
/// `resolve_ability_chain`: the drain re-enters that function, which
/// re-captures the baseline at entry either way. That mutation is pinned by
/// `until_stop_conditions_with_a_tracked_non_pausing_body_draws_after_progress`
/// at the bottom of this file.
#[test]
fn tainted_pact_draws_when_the_library_empties_mid_repeat() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let only_card = scenario
        .add_spell_to_library_top(P0, "Only Card", true)
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());

    let mut runner = scenario.build();
    put_library_top(&mut runner, only_card);
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        1,
        "precondition: exactly one card, so iteration 2 is the stalled one"
    );

    let outcome = runner.cast(pact).decline_optional().resolve();

    assert_exile_top_resolved(outcome.events());
    // Positive reach-guard: iteration 1 really ran and really exiled, so the
    // draw assertions below are not vacuous.
    outcome.assert_zone(&[only_card], Zone::Exile);
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "a repeat that stalls on mandatory actions after making progress is a \
         draw (CR 104.4b), got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        draw_event_count(outcome.events()),
        1,
        "the resumed iteration's draw event must reach the action result"
    );
    assert!(
        outcome.state().active_repeat_until().is_none(),
        "the repeat-until frame must retire, not stay parked"
    );
}

/// CR 104.1 + CR 104.4b + CR 603.3b: no trigger-ordering prompt opens after the
/// draw.
///
/// Tainted Pact draws mid-resolution and is then put into P0's graveyard
/// (CR 608.2n). P1 controls two different "whenever a card is put into an
/// opponent's graveyard" triggers. The game ended with the draw (CR 104.1), so
/// the post-action pipeline must not process them: `run_post_action_pipeline`
/// skips `process_triggers` once `GameState::game_end` is recorded. Without that
/// guard the pipeline opens P1's ordering prompt over the draw's wait. The
/// recorded result is then still restored at `reconcile_terminal_result`, so
/// the game still ends in a draw, but `pending_trigger_order` is left staged
/// for a game that is over.
#[test]
fn tainted_pact_draw_opens_no_trigger_ordering_prompt() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());
    let haunt = scenario
        .add_creature(P1, "The Haunt of Hightower", 3, 3)
        .from_oracle_text_with_keywords(&["Flying", "Lifelink"], HAUNT_OF_HIGHTOWER_ORACLE)
        .id();
    let ascension = scenario
        .add_enchantment_from_oracle(P1, "Bloodchief Ascension", BLOODCHIEF_ASCENSION_ORACLE)
        .id();
    // CR 603.4: the Ascension's graveyard trigger checks its quest counters as
    // it triggers.
    scenario.with_counter(ascension, CounterType::Generic("quest".to_string()), 3);

    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: this regression is about a starved producer"
    );
    // Reach-guard: BOTH of P1's oracle lines really parsed into the
    // opponent-graveyard trigger this row assumes, so CR 603.3b genuinely had
    // two triggers to order. Without it `pending_trigger_order.is_none()` and
    // `stack.is_empty()` below would pass on a board with nothing orderable.
    let orderable = assert_parses_to_opponent_graveyard_trigger(runner.state(), haunt, P1)
        + assert_parses_to_opponent_graveyard_trigger(runner.state(), ascension, P1);
    assert_eq!(
        orderable, 2,
        "reach-guard: CR 603.3b raises an ORDERING prompt only when one player \
         controls two or more triggers on the same event"
    );

    let outcome = runner.cast(pact).resolve();

    assert_exile_top_resolved(outcome.events());
    // Reach-guard: the Pact really went to P0's graveyard after the draw, the
    // event both of P1's triggers watch for.
    outcome.assert_zone(&[pact], Zone::Graveyard);
    assert!(
        outcome.state().pending_trigger_order.is_none(),
        "no CR 603.3b ordering prompt may open after the game ended (CR 104.1)"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "no trigger may go on the stack after the game ended (CR 104.1)"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "the action must finish on the draw (CR 104.1), got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(draw_event_count(outcome.events()), 1);
}

/// CR 104.1 + CR 104.4b: no trigger goes on the stack after the draw.
///
/// The single-trigger sibling of the row above. P1's lone "whenever a card is
/// put into an opponent's graveyard" trigger needs no ordering choice, so
/// without the `game_end` guard in `run_post_action_pipeline` the pipeline puts
/// it straight onto the stack of a game that is over. The wait stays on the
/// draw, so the harness stops there, but the trigger is left on the stack.
#[test]
fn tainted_pact_draw_puts_no_trigger_on_the_stack() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());
    let haunt = scenario
        .add_creature(P1, "The Haunt of Hightower", 3, 3)
        .from_oracle_text_with_keywords(&["Flying", "Lifelink"], HAUNT_OF_HIGHTOWER_ORACLE)
        .id();

    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: this regression is about a starved producer"
    );
    // Reach-guard: the Haunt's oracle line really parsed into the
    // opponent-graveyard trigger this row assumes, and that trigger really
    // places a +1/+1 counter on itself. Without it both `stack.is_empty()` and
    // the `Plus1Plus1` check below would pass on a board where the line stopped
    // producing a trigger, or produced one that never could have placed that
    // counter.
    {
        assert_eq!(
            assert_parses_to_opponent_graveyard_trigger(runner.state(), haunt, P1),
            1,
            "reach-guard: exactly one graveyard trigger, so no CR 603.3b \
             ordering choice stands between the event and the stack"
        );
        let execute = runner.state().objects[&haunt]
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| entry.definition())
            .find(|def| matches!(def.mode, TriggerMode::ChangesZone))
            .and_then(|def| def.execute.as_deref())
            .expect("reach-guard: the Haunt's graveyard trigger must have an execute chain");
        assert!(
            matches!(
                *execute.effect,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "reach-guard: the Haunt's graveyard trigger must parse to a +1/+1 \
             counter on itself — the exact counter the assertion below pins as \
             ABSENT, got {execute:?}"
        );
    }

    let outcome = runner.cast(pact).resolve();

    assert_exile_top_resolved(outcome.events());
    // Reach-guard: the Pact really went to P0's graveyard after the draw, the
    // event the Haunt's trigger watches for.
    outcome.assert_zone(&[pact], Zone::Graveyard);
    assert!(
        outcome.state().stack.is_empty(),
        "the Haunt's trigger must not go on the stack after the game ended \
         (CR 104.1), stack = {:?}",
        outcome.state().stack
    );
    assert_eq!(
        outcome.state().objects[&haunt]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        None,
        "nothing may happen in the game after it ended"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "the action must finish on the draw (CR 104.1), got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(draw_event_count(outcome.events()), 1);
}

/// CR 104.1 + CR 616.1: the draw also survives a replacement-order prompt on
/// Tainted Pact's own move to the graveyard. Leyline of the Void and Rest in
/// Peace, both controlled by P1, each want to exile it instead, so its owner
/// chooses which applies first. That choice is raised inside the spell's
/// resolution, after the draw, and parks `waiting_for` on the prompt.
#[test]
fn tainted_pact_draw_survives_a_replacement_order_prompt_on_its_graveyard_move() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());
    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID_ORACLE);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE_ORACLE);

    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: this regression is about a starved producer"
    );

    let outcome = runner.cast(pact).resolve();

    assert_exile_top_resolved(outcome.events());
    assert!(
        outcome.state().pending_replacement.is_some(),
        "reach guard: the two exile-instead redirects must have parked a CR 616.1 \
         order choice on Tainted Pact's graveyard move"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "a replacement-order prompt after the draw must not undo it (CR 104.1), got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        draw_event_count(outcome.events()),
        1,
        "restoring the draw must not announce it a second time"
    );
}

/// Control for the draw rows above: a Tainted Pact whose repeat CAN stop
/// resolves normally. The single card is exiled and the put is accepted, which
/// meets the "until you put a card into your hand" condition (CR 608.2c).
#[test]
fn tainted_pact_that_puts_a_card_into_hand_resolves_without_a_draw() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let only_card = scenario
        .add_spell_to_library_top(P0, "Only Card", true)
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());

    let mut runner = scenario.build();
    put_library_top(&mut runner, only_card);

    let outcome = runner.cast(pact).accept_optional().resolve();

    assert_exile_top_resolved(outcome.events());
    outcome.assert_zone(&[only_card], Zone::Hand);
    outcome.assert_zone(&[pact], Zone::Graveyard);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "a repeat that met its stop condition ends normally, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(draw_event_count(outcome.events()), 0);
    assert!(outcome.state().active_repeat_until().is_none());
}

/// Control for the draw rows above, through the cast pipeline: exiling two
/// cards with the same name stops the repeat (CR 608.2c) and the game goes on.
#[test]
fn tainted_pact_that_exiles_a_duplicate_name_resolves_without_a_draw() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolt_a = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let island = scenario.add_spell_to_library_top(P0, "Island", true).id();
    let bolt_b = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());

    let mut runner = scenario.build();
    // Exile order: bolt_b, island, bolt_a.
    put_library_top(&mut runner, bolt_b);

    let outcome = runner.cast(pact).decline_optional().resolve();

    outcome.assert_zone(&[bolt_b, island, bolt_a], Zone::Exile);
    outcome.assert_zone(&[pact], Zone::Graveyard);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the duplicate-name stop ends the repeat normally, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(draw_event_count(outcome.events()), 0);
    assert!(outcome.state().active_repeat_until().is_none());
}

/// CR 104.4b carve-out: "Loops that contain an optional action don't result in
/// a draw." A stalled iteration that offered a real "you may" (here a feasible
/// optional life gain after an `ExileTop` on an empty library) ends the
/// repeat instead of drawing, even though nothing it did can meet a stop
/// condition.
///
/// Revert-failing against a verdict that draws on any stalled iteration: the
/// decline resumes through `drain_active_repeat_until`, which must classify the
/// paused iteration as having offered an optional action.
#[test]
fn stalled_repeat_that_offered_an_optional_action_is_not_a_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_spell_to_graveyard(P0, "Optional Repeat Source", true)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: the producer is starved, so the first iteration stalls"
    );

    let mut ability = ResolvedAbility::new(
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
            face_down: false,
        },
        vec![],
        source,
        P0,
    );
    let mut gain = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    gain.optional = true;
    ability.sub_ability = Some(Box::new(gain));
    ability.repeat_until = Some(RepeatContinuation::UntilStopConditions {
        stop_on_put_to_hand: true,
        stop_on_duplicate_exiled_names: false,
    });

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: the optional life gain must be offered, got {:?}",
        runner.state().waiting_for
    );

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("optional decision");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a loop containing an optional action is not a draw, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(draw_event_count(&result.events), 0);
    assert!(runner.state().active_repeat_until().is_none());
}

/// CR 104.4b + CR 732.4: the IN-LOOP verdict arm, at the variant level.
///
/// This fixture gives the repeat a body that CANNOT pause — bare `ExileTop`,
/// no optional sub-ability — against an empty library: the shape an
/// empty-library Tainted Pact has once its impossible "you may" is no longer
/// offered. Every iteration is the same mandatory no-op, so the in-loop arm
/// must declare a draw. Without it the `loop` in `resolve_ability_chain`'s
/// `UntilStopConditions` arm never yields and never returns.
///
/// Deliberately NOT a `/card-test` cast-pipeline row: no card in the corpus
/// prints this body, and `drive_resolution`'s 64-iteration bound only helps if
/// the engine yields between iterations — here it never does, so the spin would
/// happen inside a single `resolve()` call with the bound never consulted. The
/// skill's "never call the raw `resolve()` stack function directly" rule names
/// `stack::resolve_top` / `effect::resolve` and exists to preserve the
/// intervening-if recheck and the `cast_from_zone` carry-through; this fixture
/// never puts anything on the stack and never casts, so that rule has no
/// subject here.
///
/// BOUNDED HARNESS: the work runs on a spawned thread and the assertion waits
/// on `recv_timeout`, so a missing or incorrect guard FAILS instead of hanging
/// the suite. Residual, stated honestly: `recv_timeout` returning does not stop
/// the spawned thread. Under nextest's process-per-test isolation
/// (`.config/nextest.toml`, profile `ci`) the leaked thread dies with this
/// test's own process; under plain `cargo test`, which shares one process per
/// test binary, it keeps spinning and allocating until the binary exits.
#[test]
fn until_stop_conditions_with_a_non_pausing_body_draws_in_loop() {
    let (tx, rx) = mpsc::channel();
    let _fixture = std::thread::spawn(move || {
        let mut state = GameState::new_two_player(2899);
        let mut ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
                face_down: false,
            },
            vec![],
            ObjectId(900),
            P0,
        );
        ability.repeat_until = Some(RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand: true,
            stop_on_duplicate_exiled_names: false,
        });

        let mut events = Vec::new();
        let ok = resolve_ability_chain(&mut state, &ability, &mut events, 0).is_ok();
        let exile_tops = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::ExileTop,
                        ..
                    }
                )
            })
            .count();
        let _ = tx.send((
            ok,
            events.len(),
            exile_tops,
            draw_event_count(&events),
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            format!("{:?}", state.waiting_for),
            state.active_repeat_until().is_some(),
        ));
    });

    let (ok, event_count, exile_tops, draws, is_draw, waiting_for, frame_still_parked) = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|err| {
            panic!(
                "the UntilStopConditions repeat never returned ({err:?}): the in-loop \
                 `repeat_until_verdict` arm in `resolve_ability_chain`'s \
                 UntilStopConditions dispatch is missing or incorrect, so the loop \
                 spins without ever yielding a WaitingFor"
            )
        });

    assert!(ok, "the repeat must resolve cleanly, not error out");
    // Positive reach-guard: a body that failed to build would also "return".
    assert!(
        exile_tops >= 1,
        "reach-guard: the repeat body must have resolved at least one ExileTop"
    );
    assert!(
        event_count < 64,
        "a terminating repeat emits a bounded event list, got {event_count}"
    );
    assert!(
        is_draw,
        "a mandatory no-op loop is a draw (CR 104.4b), got {waiting_for}"
    );
    assert_eq!(draws, 1, "the draw must be announced exactly once");
    assert!(
        !frame_still_parked,
        "a non-pausing body must never park a repeat-until frame"
    );
}

/// CR 104.4b: the in-loop verdict's baseline is captured PER ITERATION, not
/// once per repeat — the sibling of the test above, and the only row that pins
/// that.
///
/// WHICH MUTATION THIS TEST PINS, stated exactly: hoisting
/// `resolve_ability_chain`'s `let progress_baseline = …repeat_until_stop_witness(…)`
/// out of the `UntilStopConditions` `loop` and above it. That turns the guard
/// into "nothing has been exiled since the repeat BEGAN", a strictly weaker
/// predicate, and this test then spins until its `recv_timeout` and FAILS.
///
/// NOTHING ELSE IN THIS FILE PINS IT.
/// `tainted_pact_draws_when_the_library_empties_mid_repeat` has the same
/// progress-then-stall SHAPE but its first iteration pauses and resumes through
/// `drain_active_repeat_until`, which re-enters `resolve_ability_chain` and so
/// re-captures the baseline at function entry either way — the hoist is
/// invisible from there.
/// `until_stop_conditions_with_a_non_pausing_body_draws_in_loop` does reach
/// the in-loop arm, but its ability carries NO linked-exile consumer, so
/// `exile_links::should_track_exiled_by_source` is false, neither ledger is ever
/// written, and its witness is empty on every iteration — a hoisted baseline is
/// also empty, so the comparison is unchanged and the hoist is invisible there
/// too. Only a body that is BOTH non-pausing AND tracked discriminates.
///
/// The fixture: `ExileTop` chained to a genuine linked-exile consumer
/// (`QuantityRef::CardsExiledBySource` — "gain 1 life for each card exiled this
/// way"), which makes `should_track_exiled_by_source` true so the ledgers
/// actually grow, and which neither pauses nor moves a card out of exile. Run
/// against a ONE-CARD library: iteration 1 exiles the card and grows the
/// witness; iteration 2 finds the library empty, changes nothing, and must draw
/// via the in-loop `repeat_until_verdict` arm (no optional action anywhere in
/// the body). With the baseline hoisted, iteration 2's witness (one row) never
/// equals the repeat's start (empty), `should_stop_repeat_until` stays false
/// because the card is in exile and not in hand, and the loop never ends.
///
/// The draw also proves the in-loop arm ended the repeat: the drain can only
/// run behind a player action, and this fixture takes none.
///
/// Same bounded `std::thread` + `recv_timeout` harness as the test above, and
/// the same residual: `recv_timeout` returning does not stop the spawned
/// thread. Under nextest's process-per-test isolation the leaked thread dies
/// with this test's own process; under plain `cargo test` it keeps spinning and
/// allocating until the binary exits.
#[test]
fn until_stop_conditions_with_a_tracked_non_pausing_body_draws_after_progress() {
    let (tx, rx) = mpsc::channel();
    let _fixture = std::thread::spawn(move || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let only_card = scenario
            .add_spell_to_library_top(P0, "Only Card", true)
            .id();
        let source = scenario
            .add_spell_to_graveyard(P0, "Tracked Repeat Source", true)
            .id();
        let mut runner = scenario.build();
        assert_eq!(
            runner.state().players[P0.0 as usize]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![only_card],
            "precondition: exactly one card, so iteration 2 is the stalled one"
        );

        let mut ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
                face_down: false,
            },
            vec![],
            source,
            P0,
        );
        // CR 607.1: the linked-exile consumer that makes
        // `should_track_exiled_by_source` true, so the exile ledgers — and
        // therefore the witness — actually grow on iteration 1. It reads the
        // linked pool without pausing and without moving anything out of exile.
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )));
        ability.repeat_until = Some(RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand: true,
            stop_on_duplicate_exiled_names: false,
        });

        let mut events = Vec::new();
        let ok = resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).is_ok();
        let state = runner.state();
        let tracked_this_turn = state
            .cards_exiled_with_source_this_turn
            .get(&source)
            .map_or(0, Vec::len);
        let linked = state
            .exile_links
            .iter()
            .filter(|link| link.source_id == source)
            .count();
        let _ = tx.send((
            ok,
            events.len(),
            state.objects.get(&only_card).map(|obj| obj.zone),
            tracked_this_turn,
            linked,
            state.active_repeat_until().is_some(),
            draw_event_count(&events),
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            format!("{:?}", state.waiting_for),
        ));
    });

    let (
        ok,
        event_count,
        card_zone,
        tracked_this_turn,
        linked,
        frame_still_parked,
        draws,
        is_draw,
        waiting_for,
    ) = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|err| {
            panic!(
                "the tracked UntilStopConditions repeat never returned ({err:?}): the \
                     `progress_baseline` capture in `resolve_ability_chain`'s \
                     UntilStopConditions arm must happen INSIDE the loop, once per \
                     iteration. Hoisted above the loop it measures against the repeat's \
                     start, so an iteration that stalls AFTER making progress never \
                     compares equal and the loop never ends"
            )
        });

    assert!(ok, "the repeat must resolve cleanly, not error out");
    // Positive reach-guards: iteration 1 really exiled, and really TRACKED what
    // it exiled — without both, the witness is empty every iteration and this
    // test degenerates into the non-tracked sibling above.
    assert_eq!(
        card_zone,
        Some(Zone::Exile),
        "reach-guard: iteration 1 must actually exile the only card"
    );
    assert_eq!(
        (tracked_this_turn, linked),
        (1, 1),
        "reach-guard: the linked-exile consumer must make both ledgers record \
         the exiled card, so the witness genuinely GREW on iteration 1"
    );
    assert!(
        event_count < 64,
        "a terminating repeat emits a bounded event list, got {event_count}"
    );
    assert!(
        !frame_still_parked,
        "a non-pausing body must never park a repeat-until frame"
    );
    assert!(
        is_draw,
        "a repeat that stalls on mandatory actions after progress is a draw \
         (CR 104.4b), got {waiting_for}"
    );
    assert_eq!(draws, 1, "the draw must be announced exactly once");
}

/// CR 104.4b + CR 608.2c: a stalled witness does not prove a mandatory loop
/// when the iteration moved an object the witness does not count.
///
/// The body is `until_stop_conditions_with_a_non_pausing_body_draws_in_loop`'s
/// bare `ExileTop` with no linked-exile consumer, run against a ONE-CARD
/// library. `exile_links::should_track_exiled_by_source` is false, so iteration
/// 1 exiles the card without writing either ledger and the witness is
/// unchanged. That iteration made progress, so it is not a CR 104.4b loop. The
/// in-loop verdict sees the `ZoneChanged` and ends the process (the deliberate
/// `Stop` bound documented on `repeat_until_verdict`). Without the movement
/// input the verdict reads the unchanged witness as a mandatory no-op and
/// declares a draw on iteration 1, with a card just exiled.
///
/// Plain harness, no thread: every verdict this fixture can reach returns. A
/// verdict that repeated after the move would find the library empty on
/// iteration 2 and draw there.
#[test]
fn until_stop_conditions_that_moves_an_untracked_card_stops_without_a_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let only_card = scenario
        .add_spell_to_library_top(P0, "Only Card", true)
        .id();
    let source = scenario
        .add_spell_to_graveyard(P0, "Untracked Repeat Source", true)
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![only_card],
        "precondition: exactly one card, so iteration 1 is the one that moves it"
    );

    let mut ability = ResolvedAbility::new(
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
            face_down: false,
        },
        vec![],
        source,
        P0,
    );
    ability.repeat_until = Some(RepeatContinuation::UntilStopConditions {
        stop_on_put_to_hand: true,
        stop_on_duplicate_exiled_names: false,
    });

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();
    let state = runner.state();

    // Reach-guards: iteration 1 really moved the card, and the witness really
    // did not see it. Without the second, the witness would grow and the
    // verdict would repeat instead of reaching the stalled case under test.
    assert_eq!(
        state.objects.get(&only_card).map(|obj| obj.zone),
        Some(Zone::Exile),
        "reach-guard: iteration 1 must exile the only card"
    );
    assert_eq!(
        (
            state
                .cards_exiled_with_source_this_turn
                .get(&source)
                .map_or(0, Vec::len),
            state
                .exile_links
                .iter()
                .filter(|link| link.source_id == source)
                .count(),
        ),
        (0, 0),
        "reach-guard: with no linked-exile consumer neither ledger records the \
         card, so the witness stalls on an iteration that moved it"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "an iteration that moved a card is not a mandatory no-op loop, got {:?}",
        state.waiting_for
    );
    assert_eq!(draw_event_count(&events), 0);
    let exile_tops = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ExileTop,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        exile_tops, 1,
        "the process ends after the iteration that moved the card"
    );
    assert!(state.active_repeat_until().is_none());
}

/// CR 608.2c + CR 609.3 (issue #8798): a MANDATORY "put that card into your
/// hand" after an `ExileTop` that exiled nothing moves nothing. Without the
/// `ChangeZone` missing-referent guard, the unresolved `ParentTarget` falls back
/// to the ability's source and puts the source card into its owner's hand.
///
/// This is the guard's own row: the optional Tainted Pact rider never reaches
/// `change_zone::resolve` with a missing referent, because the feasibility
/// probe auto-declines it first.
#[test]
fn parent_target_move_after_an_empty_exile_top_moves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_spell_to_graveyard(P0, "Exile Top Source", true)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: the ExileTop has nothing to exile"
    );

    let mut ability = ResolvedAbility::new(
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
            face_down: false,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Hand,
            target: TargetFilter::ParentTarget,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        P0,
    )));

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeZone,
                ..
            }
        )),
        "reach guard: the chained move must have resolved"
    );
    assert_eq!(
        runner.state().objects.get(&source).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "with no exiled card, \"that card\" must not re-bind to the source"
    );
}

/// CR 608.2c + CR 609.3 (issue #8798's class, `Dig` parent): Jace, the Living
/// Guildpact's +1 with an empty library. "Look at the top two cards of your
/// library" looks at nothing, so the mandatory "Put one of them into your
/// graveyard" has no "them" and moves nothing. The +1 cost is still paid
/// (CR 606.4).
///
/// The +1 parses to `Dig { keep_count: Some(0) }` followed by a mandatory
/// `ChangeZone { ParentTarget → Graveyard }`. An empty `Dig` stamps
/// `ParentTargetMissingReason::Dig` onto that move, and `change_zone::resolve`
/// turns it into a no-op. Without that guard the unresolved `ParentTarget`
/// falls back to the ability's source, and Jace puts himself into his owner's
/// graveyard.
#[test]
fn jace_plus_one_on_an_empty_library_moves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, the Living Guildpact",
            "Jace",
            5,
            JACE_THE_LIVING_GUILDPACT_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: the +1 has nothing to look at"
    );
    // Reach-guard: the +1 is the `Dig` → `ParentTarget` move shape under test.
    let plus_one = &runner.state().objects[&jace].abilities[0];
    assert!(
        matches!(
            *plus_one.effect,
            Effect::Dig {
                keep_count: Some(0),
                ..
            }
        ) && plus_one.sub_ability.as_ref().is_some_and(|sub| matches!(
            *sub.effect,
            Effect::ChangeZone {
                destination: Zone::Graveyard,
                target: TargetFilter::ParentTarget,
                ..
            }
        ) && !sub.optional),
        "reach-guard: the +1 must parse to Dig then a mandatory ParentTarget \
         move to the graveyard, got {plus_one:?}"
    );

    let outcome = runner.activate(jace, 0).resolve();

    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeZone,
                ..
            }
        )),
        "reach-guard: the chained move must have resolved"
    );
    assert_eq!(
        outcome.zone_of(jace),
        Zone::Battlefield,
        "\"one of them\" must not re-bind to Jace"
    );
    let jace_obj = &outcome.state().objects[&jace];
    assert_eq!(jace_obj.loyalty, Some(6), "the +1 cost is paid");
    assert_eq!(
        jace_obj.counters.get(&CounterType::Loyalty).copied(),
        Some(6)
    );
    assert!(
        !outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                to: Zone::Graveyard,
                ..
            }
        )),
        "nothing may be put into a graveyard"
    );
    assert!(outcome.state().players[P0.0 as usize].graveyard.is_empty());
}

/// CR 608.2c + CR 608.2d + CR 609.3 (issue #8798's class, outside Tainted
/// Pact): Fishing Gear's trigger resolves against a player whose library is
/// empty. "Exile the top card of that player's library" exiles nothing, so
/// "you may put it onto the battlefield under your control" has no card to
/// put. It is declined without being offered, and "If you don't, create a 1/1
/// blue Fish creature token" creates the Fish.
///
/// The Fish alone does not pin the `ChangeZone` arm of
/// `auto_decline_infeasible_optional`: without that arm the impossible put is
/// still not offered, but it is executed, and the `ChangeZone`
/// missing-referent guard turns it into a resolved no-op after which the
/// "If you don't" branch also runs. What pins the arm is the absent
/// `EffectResolved { kind: ChangeZone }`: a declined instruction is not
/// performed at all (CR 608.2d).
#[test]
fn fishing_gear_on_an_empty_library_declines_the_put_and_makes_a_fish() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bearer = scenario.add_creature(P0, "Gear Bearer", 1, 1).id();
    let gear = scenario
        .add_creature(P0, "Fishing Gear", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(FISHING_GEAR_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), gear, bearer);
    evaluate_layers(runner.state_mut());
    assert!(
        runner.state().players[P1.0 as usize].library.is_empty(),
        "precondition: the damaged player's library is empty"
    );
    // Reach-guard: the trigger really parsed with the OPTIONAL
    // `ChangeZone { ParentTarget -> Battlefield }` arm. The absent
    // `EffectResolved { kind: ChangeZone }` assertion at the bottom of this row
    // exists to pin that arm as DECLINED and therefore not performed at all
    // (CR 608.2d) — it would pass for the wrong reason if the arm never parsed,
    // and the `fish == 1` check gives only partial reach because "If you don't"
    // also fires when the put resolves as a no-op.
    {
        let execute = runner.state().objects[&gear]
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| entry.definition())
            .find(|def| matches!(def.mode, TriggerMode::DamageDone))
            .and_then(|def| def.execute.as_deref())
            .expect(
                "reach-guard: Fishing Gear's combat-damage line must parse to a \
                 DamageDone trigger with an execute chain",
            );
        assert!(
            !chain_contains_unimplemented(execute),
            "reach-guard: Fishing Gear's trigger must parse with no \
             Effect::Unimplemented, got {execute:?}"
        );
        let put = execute
            .sub_ability
            .as_deref()
            .expect("reach-guard: the \"you may put it\" rider must parse as a sub-ability");
        assert!(
            matches!(
                *put.effect,
                Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ) && put.optional,
            "reach-guard: Fishing Gear's trigger must carry the OPTIONAL \
             ChangeZone {{ ParentTarget -> Battlefield }} arm that the \
             declined-not-resolved assertion below pins (CR 608.2d), got {put:?}"
        );
    }

    run_combat(&mut runner, vec![bearer], vec![]);
    assert!(
        !runner.state().stack.is_empty(),
        "reach guard: Fishing Gear's combat-damage trigger must be on the stack"
    );

    // Bounded: both players pass once per resolution (CR 117.4).
    let mut events = Vec::new();
    for _ in 0..8 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
    }

    assert_exile_top_resolved(&events);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "the impossible put must not be offered, got {:?}",
        runner.state().waiting_for
    );
    let fish = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.name == "Fish" && obj.controller == P0)
        .count();
    assert_eq!(fish, 1, "\"If you don't\" must create the Fish");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeZone,
                ..
            }
        )),
        "the impossible put must be declined, not resolved as a no-op"
    );
}
