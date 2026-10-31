//! CR 514.3a + CR 603.3b + CR 603.10d: the abilities a cleanup-step control
//! reversion triggers are put on the stack in that cleanup step, through a
//! repeated cleanup step, and not at the next turn's upkeep.
//!
//! CR 514.3a: "At this point, the game checks to see if any state-based actions
//! would be performed and/or any triggered abilities are waiting to be put onto
//! the stack … If so, those state-based actions are performed, then those
//! triggered abilities are put on the stack, then the active player gets
//! priority." CR 514.1's discard and CR 514.2's damage removal and
//! "until end of turn" expiry come first; the abilities are placed only
//! afterwards, as one CR 603.3b batch.
//!
//! Every row drives the real priority/cleanup pipeline (`GameRunner::act` →
//! `priority::handle_priority_pass_with_limit` → `turns::auto_advance` →
//! `turns::execute_cleanup`, then `GameAction::OrderTriggers` /
//! `GameAction::SelectCards`). The cards are built from their verbatim Oracle
//! text; the printed trigger is the one CR 603.10d attributes to the player who
//! lost control.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::DelayedTriggerCondition;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const KHARN: &str = "Berzerker \u{2014} Kh\u{e2}rn the Betrayer attacks or blocks each combat if \
    able.\nSigil of Corruption \u{2014} When you lose control of Kh\u{e2}rn the Betrayer, draw two \
    cards.\nThe Betrayer \u{2014} If damage would be dealt to Kh\u{e2}rn the Betrayer, prevent that \
    damage and an opponent of your choice gains control of it.";
/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
    creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't \
    cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and \
    gain control of it until end of turn. It gains haste until end of turn.";
/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const STOLEN_UNIFORM: &str = "Choose target creature you control and target Equipment. Gain \
    control of that Equipment until end of turn. Attach it to the chosen creature. When you lose \
    control of that Equipment this turn, if it's attached to a creature you control, unattach it.";
/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const GUSTHAS_SCEPTER: &str = "{T}: Exile a card from your hand face down. You may look at it for \
    as long as it remains exiled.\n{T}: Return a card you own exiled with this artifact to your \
    hand.\nWhen you lose control of this artifact, put all cards exiled with this artifact into \
    their owner's graveyard.";
/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const BONE_SAW: &str = "Equipped creature gets +1/+0.\nEquip {1} ({1}: Attach to target creature \
    you control. Equip only as a sorcery.)";
/// Verbatim Oracle text (MTGJSON `AtomicCards.json`).
const WAYLAY: &str = "Create three 2/2 white Knight creature tokens. Exile them at the beginning \
    of the next cleanup step.";

/// Both players' libraries, so a draw never decks anyone out (CR 704.5b).
const LIBRARY: [&str; 12] = [
    "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8", "Lib 9", "Lib 10",
    "Lib 11", "Lib 12",
];

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn add_zero_cost_spell(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    instant: bool,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn add_kharn(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Kh\u{e2}rn the Betrayer", 5, 1, KHARN)
        .as_legendary()
        .id()
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Passes priority while the same turn is in progress and cleanup has not been
/// reached, and returns the turn number at the call. Callers assert the Cleanup
/// landing unconditionally: on the unfixed engine the end-step pass crosses into
/// the next turn's upkeep in one action, so the assertion fails instead of the
/// loop hanging.
fn pass_into_cleanup(runner: &mut GameRunner) -> u32 {
    let turn_before = runner.state().turn_number;
    for _ in 0..12 {
        let state = runner.state();
        if state.phase == Phase::Cleanup || state.turn_number != turn_before {
            break;
        }
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt before cleanup: {:?}",
            state.waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("the pass must be accepted");
    }
    turn_before
}

fn stack_sources(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect()
}

/// T1 — CR 514.3a + CR 603.10d: Khârn's "when you lose control" is put on the
/// stack in the cleanup step, controlled by the player who lost control, exactly
/// once; the repeated cleanup step follows.
#[test]
fn kharn_reversion_trigger_is_stacked_in_the_cleanup_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &LIBRARY);
    }
    let kharn = add_kharn(&mut scenario, P0);
    let steal = add_zero_cost_spell(&mut scenario, P1, "Act of Treason", false, ACT_OF_TREASON);
    scenario.with_cards_in_hand(
        P1,
        &[
            "P1 Filler 1",
            "P1 Filler 2",
            "P1 Filler 3",
            "P1 Filler 4",
            "P1 Filler 5",
            "P1 Filler 6",
            "P1 Filler 7",
        ],
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    let p0_before_steal = hand_size(&runner, P0);
    runner.cast(steal).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 stole Khârn");
    assert_eq!(
        hand_size(&runner, P0),
        p0_before_steal + 2,
        "REACH: P0 lost Khârn on the steal and drew two (CR 603.10d)"
    );
    assert_eq!(hand_size(&runner, P1), 7, "seven cards at cleanup start");

    let turn_before = pass_into_cleanup(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "CR 514.3a: the reversion's trigger is stacked in the cleanup step"
    );
    assert_eq!(state.turn_number, turn_before, "still the same turn");
    assert_eq!(state.active_player, P1);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(state.stack.len(), 1, "exactly one reversion trigger");
    assert_eq!(state.stack[0].source_id, kharn);
    assert_eq!(
        state.stack[0].controller, P1,
        "CR 603.10d: the player who lost control controls the ability"
    );
    assert_eq!(controller(&runner, kharn), P0, "REACH: control reverted");
    assert_eq!(
        hand_size(&runner, P1),
        7,
        "no draw before the trigger resolves"
    );
    let p0_after_reversion = hand_size(&runner, P0);

    // CR 514.3a: the trigger resolves, then another cleanup step begins.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert_eq!(
        hand_size(&runner, P1),
        9,
        "Khârn drew exactly two for the player who lost control"
    );
    assert_eq!(
        hand_size(&runner, P0),
        p0_after_reversion,
        "P0 drew nothing"
    );
    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );

    // Passes with an empty stack repeat the cleanup step (CR 514.3a).
    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert_eq!(runner.state().turn_number, turn_before);
    assert_eq!(
        runner.state().steps_started_this_turn.count(Phase::Cleanup),
        2,
        "CR 514.3a: another cleanup step begins"
    );
    let WaitingFor::DiscardToHandSize {
        player,
        count,
        cards,
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the repeated cleanup's CR 514.1 discard: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, count), (P1, 2));
    let discard: Vec<ObjectId> = cards.iter().take(2).copied().collect();
    runner
        .act(GameAction::SelectCards { cards: discard })
        .expect("discard down to maximum hand size");

    assert_eq!(runner.state().turn_number, turn_before + 1);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert!(
        !stack_sources(&runner).contains(&kharn),
        "the trigger must not re-fire at the next turn's upkeep: {:?}",
        runner.state().stack
    );
    assert_eq!(
        hand_size(&runner, P1),
        7,
        "seven, plus Khârn's two, minus the two-card repeated-cleanup discard: seven"
    );
}

/// T2 — CR 603.3b: two reverting objects in opposite directions form one batch,
/// APNAP-ordered relative to the cleanup's active player, and each ability is
/// controlled by the player who lost control of its source.
#[test]
fn reversions_in_both_directions_stack_apnap_in_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &LIBRARY);
    }
    let kharn = add_kharn(&mut scenario, P0);
    let scepter = scenario
        .add_artifact_from_oracle(P1, "Gustha's Scepter", GUSTHAS_SCEPTER)
        .id();
    let treason = add_zero_cost_spell(&mut scenario, P1, "Act of Treason", false, ACT_OF_TREASON);
    let word = scenario
        .add_spell_to_hand(P0, "Word of Seizing", true)
        .from_oracle_text_with_keywords(&["Split second"], WORD_OF_SEIZING)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_cards_in_hand(
        P1,
        &[
            "P1 Filler 1",
            "P1 Filler 2",
            "P1 Filler 3",
            "P1 Filler 4",
            "P1 Filler 5",
            "P1 Filler 6",
            "P1 Filler 7",
        ],
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(treason).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 stole Khârn");

    // P1 passes with an empty stack; P0 takes the Scepter until end of turn.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    runner.cast(word).target_object(scepter).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, scepter),
        P0,
        "REACH: Word of Seizing took the Scepter"
    );
    assert_eq!(controller(&runner, kharn), P1, "REACH: Khârn is P1's");

    let turn_before = pass_into_cleanup(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "both reverts' triggers are stacked in the cleanup step"
    );
    assert_eq!(state.turn_number, turn_before);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(state.stack.len(), 2, "one trigger per reverted object");
    assert_eq!(
        state.stack[0].source_id, kharn,
        "the active player's trigger is placed first (bottom, CR 603.3b)"
    );
    assert_eq!(state.stack[0].controller, P1);
    assert_eq!(
        state.stack[1].source_id, scepter,
        "the nonactive player's trigger is placed last (top)"
    );
    assert_eq!(state.stack[1].controller, P0);
    assert_eq!(controller(&runner, kharn), P0, "REACH: Khârn reverted");
    assert_eq!(
        controller(&runner, scepter),
        P1,
        "REACH: the Scepter reverted"
    );

    let p1_before = hand_size(&runner, P1);
    runner.advance_until_stack_empty();
    assert_eq!(
        hand_size(&runner, P1),
        p1_before + 2,
        "Khârn's lose-control trigger drew two for P1"
    );
}

/// T3 — CR 603.3b + CR 514.2: the printed trigger and the delayed "lose
/// control of that Equipment this turn" trigger form ONE ordering batch, and
/// that batch is placed after CR 514.2's damage removal.
#[test]
fn printed_and_delayed_reversion_triggers_share_one_ordering_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &LIBRARY);
    }
    let kharn = add_kharn(&mut scenario, P0);
    let bear = scenario
        .add_creature(P1, "P1 Bear", 2, 2)
        .with_damage_marked(1)
        .id();
    let saw = scenario
        .add_artifact_from_oracle(P0, "Bone Saw", BONE_SAW)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let treason = add_zero_cost_spell(&mut scenario, P1, "Act of Treason", false, ACT_OF_TREASON);
    let uniform = add_zero_cost_spell(&mut scenario, P1, "Stolen Uniform", true, STOLEN_UNIFORM);
    scenario.with_cards_in_hand(
        P1,
        &[
            "P1 Filler 1",
            "P1 Filler 2",
            "P1 Filler 3",
            "P1 Filler 4",
            "P1 Filler 5",
            "P1 Filler 6",
            "P1 Filler 7",
        ],
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(treason).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 stole Khârn");

    runner.cast(uniform).target_objects(&[bear, saw]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, saw), P1, "REACH: P1 took the Equipment");
    assert_eq!(
        runner.state().objects[&saw].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(bear)),
        "REACH: it is attached to the chosen creature"
    );
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        1,
        "REACH: the Bear is marked before cleanup"
    );
    assert_eq!(hand_size(&runner, P1), 7, "seven cards at cleanup start");

    let turn_before = pass_into_cleanup(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "the reversion's abilities are settled in the cleanup step"
    );
    assert_eq!(state.turn_number, turn_before);
    let WaitingFor::OrderTriggers { player, triggers } = &state.waiting_for else {
        panic!(
            "CR 603.3b: one ordering choice for the whole batch: {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, P1, "both abilities are P1's");
    assert_eq!(triggers.len(), 2, "printed + delayed in one batch");
    let uniform_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == uniform)
        .expect("the Stolen Uniform delayed trigger is in the batch");
    let kharn_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == kharn)
        .expect("Khârn's printed trigger is in the batch");
    assert_ne!(uniform_index, kharn_index);
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "CR 514.2: the batch is placed after damage is removed"
    );

    // Index 0 is placed first (bottom, resolves last): put the delayed trigger
    // first and Khârn last, so Khârn's is on top and resolves first.
    runner
        .act(GameAction::OrderTriggers {
            order: vec![uniform_index, kharn_index],
        })
        .expect("submit the ordering choice");
    assert_eq!(
        stack_sources(&runner),
        vec![uniform, kharn],
        "the submitted order is placed first-to-last"
    );
    assert_eq!(
        runner.state().stack.last().map(|entry| entry.source_id),
        Some(kharn),
        "Khârn's trigger is on top (resolves first)"
    );

    let p1_before = hand_size(&runner, P1);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&saw].attached_to,
        None,
        "the delayed trigger unattached the Equipment (CR 603.4 true at the event)"
    );
    assert_eq!(
        hand_size(&runner, P1),
        p1_before + 2,
        "Khârn's lose-control trigger drew two"
    );
    assert_eq!(controller(&runner, saw), P0, "the Equipment reverted");
}

/// T4 — CR 514.1 + CR 514.3a: the discard down to maximum hand size comes
/// BEFORE the reversion's trigger goes on the stack, and the trigger is stacked
/// exactly once.
#[test]
fn kharn_reversion_with_discard_stacks_once_after_the_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &LIBRARY);
    }
    let kharn = add_kharn(&mut scenario, P0);
    let steal = add_zero_cost_spell(&mut scenario, P1, "Act of Treason", false, ACT_OF_TREASON);
    scenario.with_cards_in_hand(
        P1,
        &[
            "P1 Filler 1",
            "P1 Filler 2",
            "P1 Filler 3",
            "P1 Filler 4",
            "P1 Filler 5",
            "P1 Filler 6",
            "P1 Filler 7",
            "P1 Filler 8",
        ],
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(steal).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 stole Khârn");
    assert_eq!(hand_size(&runner, P1), 8, "eight cards at cleanup start");

    let turn_before = pass_into_cleanup(&mut runner);
    assert_eq!(runner.state().phase, Phase::Cleanup);
    let WaitingFor::DiscardToHandSize {
        player,
        count,
        cards,
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 514.1: the discard comes before the trigger: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, count), (P1, 1));
    assert!(
        runner.state().stack.is_empty(),
        "CR 514.1 before CR 514.3a: nothing is on the stack yet"
    );
    assert_eq!(
        controller(&runner, kharn),
        P0,
        "REACH: the reversion was already published"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("discard down to maximum hand size");
    let state = runner.state();
    assert_eq!(state.phase, Phase::Cleanup);
    assert_eq!(state.turn_number, turn_before);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(
        state
            .stack
            .iter()
            .filter(|entry| entry.source_id == kharn)
            .count(),
        1,
        "exactly one Khârn trigger, not a second copy from the pipeline"
    );
    assert_eq!(state.stack[0].controller, P1);

    runner.advance_until_stack_empty();
    assert_eq!(
        hand_size(&runner, P1),
        9,
        "eight minus the discard, plus two drawn"
    );
    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    let WaitingFor::DiscardToHandSize { player, count, .. } = runner.state().waiting_for else {
        panic!(
            "the repeated cleanup's discard: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, count), (P1, 2));
    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert_eq!(runner.state().turn_number, turn_before);
}

/// T7 — CR 514.3a + CR 603.7: a delayed ability waiting to be put on the stack
/// "at the beginning of the next cleanup step" is settled in that cleanup step
/// even when the control reversion which reaches the settlement block triggers
/// nothing itself. The delayed trigger's event is the step's own
/// `PhaseChanged { Cleanup }`, which lies BEFORE the reversion's published
/// events, so collecting only the reversion's range would leave it waiting past
/// cleanup.
#[test]
fn pending_next_cleanup_delayed_trigger_stacks_when_a_reversion_triggers_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &LIBRARY);
    }
    // Vanilla creature: the reversion it triggers publishes an event, but no
    // printed ability observes it.
    let bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let waylay = add_zero_cost_spell(&mut scenario, P1, "Waylay", true, WAYLAY);
    let steal = add_zero_cost_spell(&mut scenario, P1, "Act of Treason", false, ACT_OF_TREASON);
    scenario.with_cards_in_hand(
        P1,
        &[
            "P1 Filler 1",
            "P1 Filler 2",
            "P1 Filler 3",
            "P1 Filler 4",
            "P1 Filler 5",
            "P1 Filler 6",
            "P1 Filler 7",
        ],
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(waylay).resolve();
    runner.advance_until_stack_empty();
    let tokens: Vec<ObjectId> = {
        let state = runner.state();
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.controller == P1)
            .map(|object| object.id)
            .collect()
    };
    assert_eq!(tokens.len(), 3, "REACH: Waylay created three tokens");
    assert!(
        runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| matches!(
                trigger.condition,
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::Cleanup
                }
            )),
        "REACH: the next-cleanup delayed trigger is pending: {:?}",
        runner.state().delayed_triggers
    );

    runner.cast(steal).target_object(bear).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, bear), P1, "REACH: P1 stole the Bear");
    assert!(
        tokens
            .iter()
            .all(|token| runner.state().battlefield.contains(token)),
        "REACH: the tokens are still on the battlefield before cleanup"
    );
    assert!(
        runner.state().stack.is_empty(),
        "REACH: the delayed trigger has not fired before cleanup: {:?}",
        runner.state().stack
    );

    let turn_before = pass_into_cleanup(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "CR 514.3a: the next-cleanup delayed trigger is stacked in this cleanup step"
    );
    assert_eq!(state.turn_number, turn_before, "still the same turn");
    assert_eq!(state.active_player, P1);
    assert_eq!(controller(&runner, bear), P0, "REACH: control reverted");
    assert_eq!(
        stack_sources(&runner),
        vec![waylay],
        "the pending delayed trigger is the only ability on the stack"
    );
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });

    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.phase, Phase::Cleanup);
    assert_eq!(state.turn_number, turn_before);
    assert!(
        state.delayed_triggers.iter().all(|trigger| !matches!(
            trigger.condition,
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Cleanup
            }
        )),
        "the delayed ability fired once and is spent"
    );
    assert!(
        tokens
            .iter()
            .all(|token| !state.battlefield.contains(token)),
        "the delayed trigger exiled the tokens in the cleanup step"
    );
}
