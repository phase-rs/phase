//! PR-7 Phase 3 — interactive loop-shortcut protocol + APNAP response window.
//!
//! Covers the CR 732.2a/b/c live-detect bridge, `LoopDetectionMode::Interactive`, the
//! `WaitingFor::LoopShortcut`/`RespondToShortcut` states, the `DeclareShortcut`/
//! `RespondToShortcut` actions, the CR 732.4 all-mandatory no-loss draw, and the
//! conservative Shorten → priority window.
//!
//! # Golden discipline (non-circular byte-identity)
//!
//! `GOLDEN_ON` is the exact accumulated `Vec<GameEvent>` Debug string captured from HEAD
//! `dc67bd130` BEFORE the reconcile mode-`match` wrap landed (via a temporary On/Off-only
//! harness run against the UNMODIFIED reconcile body). T-ON replays the same fixture under
//! the wrapped `On` arm and asserts equality — it fails if wrapping the body in the mode
//! `match` perturbed even one event. Because the golden is pre-edit, this is not circular.

use engine::analysis::decision_template::{
    DecisionGroupKey, DecisionKind, DecisionPoint, DecisionPointKind, DecisionSlot,
    DecisionTemplate, IterationCount, PinnedDecision, ReplayMode, ShortcutDecisionSchema,
    TargetPin, TargetSchedule,
};
use engine::analysis::loop_check::{LoopCertificate, ShortcutProposal, ShortcutResponse, WinKind};
use engine::analysis::resource::{loop_states_equal_modulo_resources, BoardDelta, ResourceAxis};
use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, PersistedGameState, StackEntryKind, WaitingFor,
    YieldTarget,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const DRAIN_CLERIC: &str = "Whenever you gain life, each opponent loses 1 life.";
const BLOOD_SIPPER: &str = "Whenever an opponent loses life, you gain 1 life.";
const KICKOFF: &str = "You gain 1 life.";
const TARGETED_KICKOFF: &str = "Target player gains 1 life.";
const SELF_LIFE_ENGINE: &str = "Whenever you gain life, you gain 1 life.";
const LIFE_LOSS_IMMUNE: &str = "Your life total can't change.";
/// Symmetric self-sustaining life-loss engine: each loss event re-triggers a loss for EVERYONE.
/// The `Effect::LoseLife` AST is IDENTICAL to `DRAIN_CLERIC`'s (both `{ amount: Fixed(1), target }`);
/// the each-player/each-opponent split rides the ability-level `player_scope` (`All` vs `Opponent`),
/// and the TRIGGER differs — "a player loses life" here vs `DRAIN_CLERIC`'s "you gain life".
const PLAGUE_ENGINE: &str = "Whenever a player loses life, each player loses 1 life.";
/// Symmetric kick-off. NOT "Target player loses 1 life." — a targeted kickoff desynchronises the
/// fallers' ABSOLUTE lives and trips `fallers_lives_pairwise_equal`, so no offer is ever raised.
const LOSE_ALL_KICKOFF: &str = "Each player loses 1 life.";

// Verbatim Oracle text (data/card-data.json) for the PR-7 gate-relax witness — the
// REAL escalating TARGETED drain that only detects once item-3 (forced-unique) and
// item-4 (Typed-target projected refinement) both land.
const VITO: &str = "Whenever you gain life, target opponent loses that much life.\n{3}{B}{B}: Creatures you control gain lifelink until end of turn.";
const SANGUINE_BOND: &str = "Whenever you gain life, target opponent loses that much life.";
const BLOODTHIRSTY_CONQUEROR: &str =
    "Flying, deathtouch\nWhenever an opponent loses life, you gain that much life. (Damage causes loss of life.)";

/// The exact accumulated event Debug string of the 2p drain under `On`, captured from
/// HEAD `dc67bd130` on the UNMODIFIED reconcile body. See the module docs.
/// `subject: None` was appended to each `EffectResolved` when that field was added to the
/// event — it is `None` on every path this test drives, so the stream is otherwise unchanged.
const GOLDEN_ON: &str = r#"[StackPushed { object_id: ObjectId(3) }, ZoneChanged { object_id: ObjectId(3), from: Some(Hand), to: Stack, record: ZoneChangeRecord { object_id: ObjectId(3), name: "Test Lifegain Kickoff", core_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], trigger_definitions: [], trigger_source_context: Some(TriggerSourceContext { identity: ObjectIdentityBinding { reference: ObjectIncarnationRef { object_id: ObjectId(3), incarnation: 0 }, expected_zone: Hand }, lki: LKISnapshot { name: "Test Lifegain Kickoff", token_image_ref: None, power: None, toughness: None, base_power: None, base_toughness: None, mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), card_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], colors: [], chosen_attributes: [], counters: {}, tapped: false, is_suspected: false, attachments: [] }, card_id: CardId(3), printed_ref: None, is_token: false, face_down: false, transformed: false, is_renowned: false, is_saddled: false, class_level: None, trigger_entries: [], timestamp: 0, entered_battlefield_turn: None, paired_with: None, pair_controller: None, attached_to: None, attachments: [], linked_exile_snapshot: [], combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, cast_from_zone: None, played_from_zone: None, cast_controller: None, phase_status: PhasedIn, cast_variant_paid: None, cast_timing_permission: None, cost_x_paid: None, cast_spell_keywords: [], mana_spent_to_cast: false, colors_spent_to_cast: ColoredManaCount { white: 0, blue: 0, black: 0, red: 0, green: 0 }, mana_spent_to_cast_amount: 0, kickers_paid: [], additional_cost_payment_count: 0, additional_cost_payments: [], cast_cost_paid_object: None }), power: None, toughness: None, base_power: None, base_toughness: None, colors: [], mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), from_zone: Some(Hand), cast_from_zone: None, played_from_zone: None, to_zone: Stack, attachments: [], linked_exile_snapshot: [], is_token: false, combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, co_departed: [], entered_incarnation: None, attached_to: None, turn_zone_change_index: 0, is_suspected: false } }, SpellCast { card_id: CardId(3), controller: PlayerId(0), object_id: ObjectId(3) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(0), amount: 1 }, EffectResolved { kind: GainLife, source_id: ObjectId(3), subject: None }, ZoneChanged { object_id: ObjectId(3), from: Some(Stack), to: Graveyard, record: ZoneChangeRecord { object_id: ObjectId(3), name: "Test Lifegain Kickoff", core_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], trigger_definitions: [], trigger_source_context: Some(TriggerSourceContext { identity: ObjectIdentityBinding { reference: ObjectIncarnationRef { object_id: ObjectId(3), incarnation: 1 }, expected_zone: Stack }, lki: LKISnapshot { name: "Test Lifegain Kickoff", token_image_ref: None, power: None, toughness: None, base_power: None, base_toughness: None, mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), card_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], colors: [], chosen_attributes: [], counters: {}, tapped: false, is_suspected: false, attachments: [] }, card_id: CardId(3), printed_ref: None, is_token: false, face_down: false, transformed: false, is_renowned: false, is_saddled: false, class_level: None, trigger_entries: [], timestamp: 0, entered_battlefield_turn: None, paired_with: None, pair_controller: None, attached_to: None, attachments: [], linked_exile_snapshot: [], combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, cast_from_zone: None, played_from_zone: None, cast_controller: None, phase_status: PhasedIn, cast_variant_paid: None, cast_timing_permission: None, cost_x_paid: None, cast_spell_keywords: [], mana_spent_to_cast: false, colors_spent_to_cast: ColoredManaCount { white: 0, blue: 0, black: 0, red: 0, green: 0 }, mana_spent_to_cast_amount: 0, kickers_paid: [], additional_cost_payment_count: 0, additional_cost_payments: [], cast_cost_paid_object: None }), power: None, toughness: None, base_power: None, base_toughness: None, colors: [], mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), from_zone: Some(Stack), cast_from_zone: None, played_from_zone: None, to_zone: Graveyard, attachments: [], linked_exile_snapshot: [], is_token: false, combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, co_departed: [], entered_incarnation: None, attached_to: None, turn_zone_change_index: 1, is_suspected: false } }, StackResolved { object_id: ObjectId(3) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(1), amount: -1 }, EffectResolved { kind: LoseLife, source_id: ObjectId(1), subject: None }, StackResolved { object_id: ObjectId(4) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(0), amount: 1 }, EffectResolved { kind: GainLife, source_id: ObjectId(2), subject: None }, StackResolved { object_id: ObjectId(5) }, GameOver { winner: Some(PlayerId(0)) }]"#;

fn life(runner: &GameRunner, p: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.life)
        .unwrap()
}

fn is_eliminated(runner: &GameRunner, p: PlayerId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.is_eliminated)
        .unwrap()
}

/// 2-player self-refilling mutual drain controlled by P0 (constant-depth). P1 starts low so
/// the OFF natural-death stream is short. Returns runner + kick-off sorcery id.
fn setup_2p_drain(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 2p ESCALATING TARGETED drain (PR-7 gate-relax witness): Vito + Sanguine Bond (two
/// identical `Whenever you gain life, target opponent loses that much life` triggers) +
/// Bloodthirsty Conqueror (`Whenever an opponent loses life, you gain that much life`).
/// A seed lifegain fans out a GROWING cascade of TARGETED drains — the ω-cover path that
/// reaches item-3 (forced-unique in 2p) + item-4 (opponent `Typed` target). The two
/// identical drainers make each gain fire two simultaneous triggers ⇒ the CR 603.3b
/// OrderTriggers beat the loop-detect ring must survive.
fn setup_2p_vito(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 4, VITO);
    scenario.add_creature_from_oracle(P0, "Sanguine Bond", 2, 2, SANGUINE_BOND);
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 2-player drain (as above) but P1 also holds a castable Lightning Bolt off an untapped
/// Mountain — a meaningful priority action that makes the loop OPTIONAL (CR 732.5 probe
/// FALSE). Returns runner + (kickoff, bolt, drain-cleric enabler id).
fn setup_2p_optional_drain(mode: LoopDetectionMode) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let cleric = scenario
        .add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC)
        .id();
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    let bolt = scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff, bolt, cleric)
}

/// 3-player growing μ=2 cascade controlled by P0 (both opponents drain), P1 holding a
/// castable Bolt so the loop is OPTIONAL. The ω growing stack means the winner is confirmed
/// via `loop_states_cover_modulo_growth`, not the constant-depth equality.
fn setup_3p_optional_cascade(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 3-player MANDATORY, unstoppable, net-progress, NO-LOSS loop: P0 has a self-refilling
/// "whenever you gain life, you gain 1 life" engine. Nobody drains, nobody can break it
/// (opponents have empty hands / no abilities) ⇒ CR 732.4 draw.
fn setup_3p_draw(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 3-player SUBSET-LETHAL loop: the SAME proven-detected constant-depth mutual drain as
/// `setup_2p_drain` (P0's `DRAIN_CLERIC` + `BLOOD_SIPPER`), embedded in a 3p pod where P2 is
/// IMMUNE to life loss (CR 119.8 "you can't lose life"). So the cycle drains ONLY P1 (sole
/// faller); P2 is a bystander with per-cycle life delta 0 (a second non-faller). Living
/// partition each cycle: fallers = {P1}, non-fallers = {P0, P2} — so `live_mandatory_loop_winner`
/// refuses to name a winner (CR 104.2a). P1 starts very high so it never dies inside the drive
/// window: the test asserts the mid-loop grind (no crown), not a natural CR 704.5a death.
/// The headroom is deliberate and far exceeds `PRIMED_LOOP_BEATS` — P1 ends the capped drive
/// ~14 points below its 1000 start, hundreds of points from lethal. Do NOT trim it to match
/// the cap; the slack is what keeps the no-death premise robust to engine drift.
fn setup_3p_subset_lethal(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 1000);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_creature_from_oracle(P2, "Test Bulwark", 2, 2, LIFE_LOSS_IMMUNE);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 3-player BYSTANDER-WINNER loop. P0's `PLAGUE_ENGINE` turns any life loss into a symmetric
/// loss for everyone, so a single symmetric kick-off self-sustains. Per-cycle: P0 = -1, P1 = -1
/// (EQUAL — required by `live_mandatory_loop_winner`'s CR 704.3 simultaneity floor: fallers die in
/// ONE SBA event, so unequal lives are not a determinate single-winner shape), P2 = 0
/// (life-loss-immune: CR 101.2 — a "can't" effect takes precedence over the trigger's life-loss
/// instruction; cf. CR 119.8 for the same const elsewhere in this file). Living partition each
/// cycle: fallers = {P0, P1}, nonfallers = {P2} ⇒ len == 1 ⇒ the engine NATURALLY latches
/// `predicted_winner = Some(P2)` — a winner who controls no loop enabler and is not the proposer.
/// No injection.
///
/// P1's land + Bolt are LOAD-BEARING: they make the loop OPTIONAL (`mandatory: false`). Without
/// them the loop is mandatory and the engine auto-crowns `GameOver { winner: Some(P2) }` with no
/// offer at all (measured).
///
/// All three lives start EQUAL and high: `fallers_lives_pairwise_equal` gates on the fallers'
/// ABSOLUTE lives, not just their per-cycle deltas.
fn setup_3p_bystander_winner(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 1000);
    scenario.with_life(P1, 1000);
    scenario.with_life(P2, 1000);
    scenario.add_creature_from_oracle(P0, "Test Plague Engine", 2, 2, PLAGUE_ENGINE);
    scenario.add_creature_from_oracle(P2, "Test Bulwark", 2, 2, LIFE_LOSS_IMMUNE);
    // Optionality: P1 holds a real interactive answer ⇒ `mandatory: false` ⇒ the engine OFFERS
    // instead of auto-crowning (CR 104.4b: a loop with an optional action is not a draw either).
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Symmetric Kickoff", false, LOSE_ALL_KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// Drive PassPriority/OrderTriggers beats, accumulating events, until a state OTHER than
/// `Priority`/`OrderTriggers` (a `LoopShortcut`/`RespondToShortcut`/`GameOver`/…) or the
/// cap. Returns accumulated events + the terminal `waiting_for`.
fn drive_collect(runner: &mut GameRunner, cap: usize) -> (Vec<GameEvent>, WaitingFor) {
    let mut all: Vec<GameEvent> = Vec::new();
    for _ in 0..cap {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => match runner.act(GameAction::PassPriority) {
                Ok(r) => all.extend(r.events),
                Err(_) => break,
            },
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                match runner
                    .act(GameAction::OrderTriggers { order })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                {
                    Ok(r) => all.extend(r.events),
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }
    (all, runner.state().waiting_for.clone())
}

/// [`drive_collect`] plus a measured answer to "did the drive actually reach the board-recurrent
/// regime inside `cap`?" — the third element is `true` once some prior in
/// `GameState::loop_detect_ring` has compared equal to the live state modulo resources
/// ([`loop_states_equal_modulo_resources`], the engine's own public predicate; the test does not
/// reimplement recurrence).
///
/// Why that witnesses the classification: the sampler only pushes a prior while the stack is
/// non-empty at a `Priority` beat, and `GameState`'s `PartialEq` compares both, so a ring hit
/// forces the §3 bridge conjuncts (engine.rs) to have held at that state — i.e.
/// `find_live_loop_winner` → `live_mandatory_loop_winner` ran on it. It is deliberately STRONGER
/// than "the classifier ran": the classifier is called on every sampled beat, so a `false` here
/// does not mean it never ran — it means the loop never recurred, which is the regime every
/// no-crown assertion below assumes. Note the engine's own faller partition short-circuits
/// (`nonfallers.len() != 1`, loop_check.rs) BEFORE its recurrence gate for these subset-lethal
/// fixtures; the recurrence witnessed here is the state property, not that specific branch.
///
/// Only the equality disjunct is checked: the engine's gate is
/// `loop_states_equal_modulo_resources || loop_states_cover_modulo_growth`, and the latter is
/// `pub(crate)` (resource.rs), so an integration test cannot call it. Measured, both fixtures
/// still hit the equality disjunct; a fixture that drifted entirely into the coverability regime
/// would fail this guard and need it widened, not the cap raised.
///
/// Checked per beat, because recurrence is PHASE-dependent, not monotone: measured over caps
/// {1, 2, 3, 4, 8, 24} it holds at 2 and 8 and not at 1/3/4/24 (period 6), so inspecting only
/// the terminal state would report `false` on a perfectly primed loop. The scan short-circuits
/// at the first hit — measured 5.6–17.5 ms per drive, priming at beat 2.
fn drive_collect_primed(runner: &mut GameRunner, cap: usize) -> (Vec<GameEvent>, WaitingFor, bool) {
    let mut all: Vec<GameEvent> = Vec::new();
    let mut primed = false;
    for _ in 0..cap {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
        ) {
            break;
        }
        let (events, _) = drive_collect(runner, 1);
        all.extend(events);
        if !primed {
            let state = runner.state();
            primed = state
                .loop_detect_ring
                .iter()
                .any(|prior| loop_states_equal_modulo_resources(prior, state));
        }
    }
    (all, runner.state().waiting_for.clone(), primed)
}

// ────────────────────────────── T-OFF ──────────────────────────────

/// T-OFF: the real winning drain under `Off` reaches the natural CR 704.5a SBA death — no
/// ring sampling, no shortcut, no `ResolutionHalted`. Discriminator: the SAME fixture under
/// `Interactive` produces a DIFFERENT outcome (early shortcut, victim positive), proving
/// `Off` runs zero new code.
#[test]
fn off_natural_death_no_shortcut() {
    let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::Off);
    let out = runner.cast(kickoff).resolve();
    let mut all: Vec<GameEvent> = out.events().to_vec();
    let (rest, wf) = drive_collect(&mut runner, 2000);
    all.extend(rest);

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "OFF: the drain still ends the game for P0, via the NATURAL CR 704.5a death"
    );
    // Natural-death signature: the victim actually crossed 0 and was eliminated.
    assert!(
        life(&runner, P1) <= 0 && is_eliminated(&runner, P1),
        "OFF: P1 must have drained to <= 0 and been eliminated (no early shortcut)"
    );
    // Off runs zero new code: the ring is never populated and no shortcut/halt occurs.
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "OFF: the loop-detect ring must be empty (sampler gated off)"
    );
    assert!(
        runner.state().unbounded_resources.is_empty(),
        "OFF: no unbounded axes marked (the detector never ran)"
    );
    assert!(
        !all.iter()
            .any(|e| matches!(e, GameEvent::ResolutionHalted { .. })),
        "OFF: no ResolutionHalted — the natural death ends it cleanly"
    );

    // Discriminator: the SAME fixture under Interactive ends DIFFERENTLY (mandatory
    // winning drain → early auto-win with the victim still at positive life).
    let (mut irunner, ikickoff) = setup_2p_drain(LoopDetectionMode::Interactive);
    let _ = irunner.cast(ikickoff).resolve();
    let (_ievents, iwf) = drive_collect(&mut irunner, 500);
    assert_eq!(
        iwf,
        WaitingFor::GameOver { winner: Some(P0) },
        "Interactive: mandatory winning drain auto-wins for P0"
    );
    assert!(
        life(&irunner, P1) > 0,
        "Interactive: the shortcut fired EARLY — P1 still positive ({}), unlike OFF (<=0)",
        life(&irunner, P1)
    );
}

// ────────────────────────────── T-ON ──────────────────────────────

/// T-ON ⭐: the same lethal drain under `On`, byte-identical to the pre-PR-7 event stream
/// (`GOLDEN_ON`, captured from HEAD before the mode-`match` wrap). Fails if wrapping the
/// body perturbed even one event.
#[test]
fn on_shortcut_byte_identical_to_pre_pr7_golden() {
    let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::On);
    let out = runner.cast(kickoff).resolve();
    let mut all: Vec<GameEvent> = out.events().to_vec();
    let (rest, wf) = drive_collect(&mut runner, 500);
    all.extend(rest);

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "ON: mandatory winning drain auto-wins for P0"
    );
    assert!(
        life(&runner, P1) > 0,
        "ON: the shortcut fired early (P1 positive)"
    );
    assert_eq!(
        format!("{all:?}"),
        GOLDEN_ON,
        "ON: the accumulated event stream must be byte-identical to the pre-PR-7 golden — \
         wrapping the reconcile body in the mode `match` must not perturb any event"
    );
}

// ─────────────────────────────── T-Vito ───────────────────────────────

/// T-Vito ⭐ (PR-7 gate-relax witness): the REAL escalating TARGETED drain
/// (Vito, Thorn of the Dusk Rose + Sanguine Bond + Bloodthirsty Conqueror, verbatim
/// Oracle text) detects under `Interactive` and auto-wins for P0 with the victim still
/// at POSITIVE life — the shortcut fired EARLY (ω-cover), not a natural CR 704.5a death.
/// Detection requires BOTH item-3 (forced-unique targeted cover) AND item-4 (the
/// `Typed`-target projected refinement). The two per-conjunct revert-probes are measured
/// in the impl report: reverting EITHER conjunct loses detection ⇒ P1 grinds to natural
/// death (life <= 0) rather than an early crown. The two identical drainers exercise the
/// CR 603.3b OrderTriggers cascade beat the loop-detect ring must survive (G2).
#[test]
fn vito_bond_conqueror_2p_determinate_win() {
    let (mut runner, kickoff) = setup_2p_vito(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 2000);

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "escalating targeted drain auto-wins for P0"
    );
    // DISCRIMINATOR (flips when either conjunct is reverted): early detection leaves the
    // victim positive; losing detection grinds P1 to <= 0 via natural resolution.
    assert!(
        life(&runner, P1) > 0,
        "the shortcut fired EARLY — P1 still positive ({}); reverting item-3 or item-4 \
         loses detection and P1 reaches natural death (<=0)",
        life(&runner, P1)
    );
}

// ────────────────────────── T-3p-cascade ──────────────────────────

/// T-3p-cascade: a ≥3p growing-cascade OPTIONAL winning loop under `Interactive`. The bridge
/// OFFERS a `LoopShortcut` (not an auto-win); the proposer declares `UntilLethal`; both
/// opponents are prompted in APNAP order and Accept ⇒ `GameOver{winner: P0}`, winner via the
/// ω-covering path with the opponents still at positive life.
#[test]
fn interactive_3p_optional_cascade_apnap_accept_win() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    // The OFFER fired (NOT an auto-win): waiting on the proposer to declare the shortcut.
    assert_eq!(
        wf,
        runner.state().waiting_for.clone(),
        "drive stopped at a non-priority state"
    );
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ..
    } = wf
    else {
        panic!("Interactive optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    assert_eq!(predicted_winner, Some(P0), "the detector predicts P0 wins");
    // Fired early — both opponents alive at positive life (ω shortcut, not natural death).
    assert!(
        life(&runner, P1) > 0 && life(&runner, P2) > 0 && !is_eliminated(&runner, P1),
        "opponents must be alive at positive life when the offer fires"
    );

    // Proposer declares the shortcut.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares the shortcut");

    // APNAP fan-out: first opponent prompted, then the second, both in turn order after P0.
    let WaitingFor::RespondToShortcut {
        player: first,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("after Declare, the first opponent must be prompted");
    };
    assert_eq!(
        first, P1,
        "APNAP: first responder is the next player after P0"
    );
    assert_eq!(remaining_players, vec![P2], "APNAP: P2 queued after P1");

    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts");

    let WaitingFor::RespondToShortcut { player: second, .. } = runner.state().waiting_for.clone()
    else {
        panic!("after P1 accepts, P2 must be prompted");
    };
    assert_eq!(second, P2, "APNAP: second responder is P2");

    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P2 accepts (last) → take the shortcut");

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "both accepted ⇒ the shortcut resolves to P0's win"
    );
}

/// CR 732.2a: a shortcut belongs to the player with priority, not necessarily the player
/// whose loop will win. P1 starts the proven P0-controlled drain by making P0 gain life on
/// P1's turn, so the live bridge must offer P1 the choice while retaining P0 as the measured
/// winner. This drives the full cast → detection → authorization → APNAP → crown pipeline;
/// assigning the offer to the winner instead makes P0's intentionally unauthorized declaration
/// succeed and this test fail.
#[test]
fn interactive_offer_separates_priority_proposer_from_predicted_winner() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P1, "P0 Lifegain Kickoff", false, TARGETED_KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let _ = runner.cast(kickoff).target_player(P0).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ..
    } = wf
    else {
        panic!("P1's priority window must receive a LoopShortcut offer, got {wf:?}");
    };
    assert_eq!(
        proposer, P1,
        "CR 732.2a routes the offer to the priority holder"
    );
    assert_eq!(
        predicted_winner,
        Some(P0),
        "the public outcome remains P0's win"
    );

    let wrong = apply(
        runner.state_mut(),
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        },
    );
    assert!(
        matches!(wrong, Err(EngineError::WrongPlayer)),
        "the predicted winner cannot propose while P1 holds priority, got {wrong:?}"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P1 may propose the shortcut from its priority window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { player, .. } if player == P0
    ));
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P0 accepts the proposal that predicts its own win");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "the measured winner, not the proposer, is crowned"
    );
}

// ─────────────────────────── T-3p-draw ────────────────────────────

/// T-3p-draw: a ≥3p MANDATORY, net-progress, no-loss, unstoppable loop draws under
/// `Interactive` (CR 732.4). Discriminator: the SAME fixture under `Off` does NOT draw (it
/// grinds / halts, no §b-B branch), proving the draw is the Interactive path, not a
/// pre-existing outcome.
#[test]
fn interactive_3p_mandatory_no_loss_draw() {
    let (mut runner, kickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: None },
        "Interactive: an all-mandatory, no-loss, unstoppable net-progress loop is a CR 732.4 draw"
    );

    // Discriminator: under Off the same fixture never draws via §b-B (it grinds to the
    // iteration/growth backstop or keeps going — not GameOver{None} by this branch).
    let (mut orunner, okickoff) = setup_3p_draw(LoopDetectionMode::Off);
    let _ = orunner.cast(okickoff).resolve();
    let (_oevents, owf) = drive_collect(&mut orunner, 500);
    assert_ne!(
        owf,
        WaitingFor::GameOver { winner: None },
        "Off must NOT reach the CR 732.4 net-progress draw (that branch is Interactive-only)"
    );
}

// ────────────────────────── T-Q1-shorten ──────────────────────────

/// T-Q1-shorten ⭐: an OPTIONAL winning drain under `Interactive`. The proposer declares the
/// shortcut; the opponent SHORTENS ⇒ the engine hands THAT opponent a real priority window
/// (CR 732.2c); the opponent casts removal on an enabler ⇒ the loop breaks (no GameOver,
/// re-detection does not re-confirm). Discriminator: replacing Shorten with Accept runs the
/// same fixture to `GameOver{winner: P0}` — proving the WINDOW stopped it, not an unrelated
/// fizzle.
#[test]
fn interactive_shorten_hands_priority_and_breaks_loop() {
    let (mut runner, kickoff, bolt, cleric) =
        setup_2p_optional_drain(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0);

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares");

    // Positive reach-guard: the opponent WAS actually prompted before it responds.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "P1 must be prompted to respond before shortening"
    );

    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 1 },
        })
        .expect("P1 shortens");

    // CR 732.2c: P1 received a real priority window (not the shortcut).
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "Shorten hands the shortening opponent a priority window"
    );
    assert!(
        life(&runner, P1) > 0,
        "P1 is alive — the loop was NOT auto-taken"
    );

    // P1 casts removal on an enabler ⇒ the loop breaks.
    let _ = runner.cast(bolt).target_object(cleric).resolve();
    assert!(
        runner.state().objects.get(&cleric).map(|o| o.zone)
            != Some(engine::types::zones::Zone::Battlefield),
        "the drain enabler (Cleric) must have left the battlefield"
    );

    // Re-detection on the next beats does NOT re-confirm the (now-broken) loop.
    let (_r, wf2) = drive_collect(&mut runner, 200);
    assert!(
        !matches!(wf2, WaitingFor::GameOver { winner: Some(_) }),
        "after the enabler is removed, no player is shortcut to a win; got {wf2:?}"
    );
    assert!(
        life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
        "P1 survives — the shorten window genuinely stopped the loop"
    );

    // Discriminator: the SAME fixture with Accept instead of Shorten runs to P0's win.
    let (mut arunner, akickoff, _abolt, _acleric) =
        setup_2p_optional_drain(LoopDetectionMode::Interactive);
    let _ = arunner.cast(akickoff).resolve();
    let (_ae, awf) = drive_collect(&mut arunner, 500);
    assert!(matches!(awf, WaitingFor::LoopShortcut { .. }));
    arunner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("declare");
    arunner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        arunner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "Accept (not Shorten) ⇒ the loop resolves to P0's win — proves the window stops it"
    );
}

// ───────────────────── T-Q1-decline (Seam 1) ───────────────────────

/// T-Q1-decline ⭐ (interactive bridge, Seam 1): CR 732.2a — suggesting a shortcut is
/// OPTIONAL, so the proposer may DECLINE the auto-offered optional drain. The engine dismisses
/// the offer and restores ordinary priority to the living seat (P0 here); an ordinary action
/// then resolves and the declined loop is NOT immediately re-offered by the post-return
/// reconcile.
///
/// Non-vacuous revert-probe (measured): the interactive Seam-1 re-offer is suppressed by the
/// `apply_action` deliberate-action ring invalidation (fires for `DeclineShortcut` before the
/// handler runs). The handler therefore does NOT clear the ring itself — a per-action re-clear
/// would distrust that engine-wide invariant — so there is no handler ring-clear to serve as a
/// discriminator here. The load-bearing line for THIS test is the offer dismissal
/// (`state.waiting_for = WaitingFor::Priority { .. }`): deleting it leaves `waiting_for ==
/// LoopShortcut { P0 }` (the reconcile's Priority-gated seams skip a non-Priority state) ⇒ the
/// `Priority { P0 }` assertion (a) flips to fail. Seam-2 independence is proven by the
/// object-growth test: deleting `last_loop_action_sequence = None` fails THAT test while this one is
/// unaffected (this fixture captures no recast context).
#[test]
fn interactive_optional_drain_decline_restores_priority_no_reoffer() {
    let (mut runner, kickoff, _bolt, _cleric) =
        setup_2p_optional_drain(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    // F2 positive reach-guard: the offer was genuinely reached before we decline. Without this
    // a fixture drift that never offers would let DeclineShortcut hit the apply wildcard and
    // pass assertion (c) vacuously.
    assert!(
        matches!(wf, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "optional drain must OFFER a LoopShortcut to P0, got {wf:?}"
    );

    // CR 732.2a: the proposer (P0) declines the offer.
    let decline = runner
        .act(GameAction::DeclineShortcut)
        .expect("P0 declines the shortcut");

    // (a): the offer is dismissed and ordinary priority is restored to the living seat. Deleting
    // the handler's `state.waiting_for = Priority { .. }` dismissal leaves `waiting_for ==
    // LoopShortcut { P0 }` (the reconcile's Priority-gated seams skip a non-Priority state) ⇒
    // this assertion flips to fail — the load-bearing revert-probe for this interactive seam.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "decline dismisses the offer and restores ordinary priority to the living seat"
    );
    assert_eq!(decline.waiting_for, WaitingFor::Priority { player: P0 });
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the recurrence ring is empty after decline (invalidated by the deliberate-action clear)"
    );

    // (b) an ordinary action resolves from the restored priority window.
    runner
        .act(GameAction::PassPriority)
        .expect("an ordinary PassPriority resolves after the decline handback");

    // (c) the SAME loop is not instantly re-offered on the immediate next beat (the ring is
    // empty, so it takes several samples to re-detect; a genuine later re-recurrence would then
    // legitimately re-arm the offer — CR 732.2a event-driven re-arm).
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "the declined loop must not be re-offered on the immediate next beat, got {:?}",
        runner.state().waiting_for
    );
}

// ───────────────────── T-declare-roundtrip ─────────────────────────

/// T-declare-roundtrip: each protocol action is accepted only from its authorized actor —
/// `DeclareShortcut` from the proposer, `RespondToShortcut` from the current responder.
/// A wrong actor is rejected with `WrongPlayer`.
#[test]
fn declare_and_respond_authorization() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    assert!(matches!(wf, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0));

    // Wrong actor for DeclareShortcut (an opponent) → rejected.
    let wrong = apply(
        runner.state_mut(),
        P1,
        GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        },
    );
    assert!(
        matches!(wrong, Err(EngineError::WrongPlayer)),
        "an opponent may not declare the proposer's shortcut, got {wrong:?}"
    );

    // Correct actor (P0) → accepted; advances to the first responder.
    apply(
        runner.state_mut(),
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        },
    )
    .expect("P0 declares");
    let WaitingFor::RespondToShortcut { player: first, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected a RespondToShortcut prompt");
    };

    // Wrong actor for RespondToShortcut (the proposer) → rejected.
    let wrong2 = apply(
        runner.state_mut(),
        P0,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        },
    );
    assert!(
        matches!(wrong2, Err(EngineError::WrongPlayer)),
        "the proposer may not answer their own shortcut offer, got {wrong2:?}"
    );

    // Correct actor (the prompted opponent) → accepted.
    apply(
        runner.state_mut(),
        first,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        },
    )
    .expect("the prompted opponent accepts");

    // RIDER-2 — CR 732.2a decline authorization (fresh runner: the flow above consumed the
    // offer). `DeclineShortcut` is a normal protocol action dispatched via the
    // `(waiting_for, action)` match; `check_actor_authorization` (engine.rs:225) runs BEFORE
    // `apply_action` and keys on `WaitingFor::LoopShortcut.acting_player` == the proposer.
    // Unlike `Concede`/`Debug`, `DeclineShortcut` is NOT on any pre-match early-return
    // allowlist, so a wrong actor is rejected with the SPECIFIC `WrongPlayer` — proving the
    // decline genuinely routes THROUGH the auth firewall (a vacuous "not accepted" would also
    // pass on an allowlist bypass, which the concrete-variant assert rules out).
    let (mut drunner, dkickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = drunner.cast(dkickoff).resolve();
    let (_de, dwf) = drive_collect(&mut drunner, 500);
    assert!(
        matches!(dwf, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "decline-auth precondition: the offer must be reached with proposer P0, got {dwf:?}"
    );

    // Wrong actor for DeclineShortcut (an opponent) → the concrete WrongPlayer error.
    let wrong_decline = apply(drunner.state_mut(), P1, GameAction::DeclineShortcut);
    assert!(
        matches!(wrong_decline, Err(EngineError::WrongPlayer)),
        "an opponent may not decline the proposer's shortcut, got {wrong_decline:?}"
    );
    // The rejected action left the offer intact (no state mutation on an auth reject).
    assert!(
        matches!(drunner.state().waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "a rejected wrong-actor decline must not disturb the offer, got {:?}",
        drunner.state().waiting_for
    );

    // Correct actor (the proposer P0) → accepted; ordinary priority handed back.
    apply(drunner.state_mut(), P0, GameAction::DeclineShortcut).expect("P0 declines");
    assert!(
        matches!(drunner.state().waiting_for, WaitingFor::Priority { .. }),
        "the proposer's decline hands ordinary priority back, got {:?}",
        drunner.state().waiting_for
    );
}

// ─────────────────── T-variant-housekeeping ────────────────────────

/// T-variant-housekeeping: `WaitingFor::LoopShortcut{proposer}.acting_player()` reads the
/// `proposer` field (routing authorization to the proposer), not a constant.
#[test]
fn loop_shortcut_acting_player_reads_proposer() {
    let cert = LoopCertificate {
        unbounded: vec![],
        win_kind: WinKind::LethalDamage,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
    };
    let wf_a = WaitingFor::LoopShortcut {
        proposer: P1,
        predicted_winner: Some(P0),
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
    };
    let wf_b = WaitingFor::LoopShortcut {
        proposer: P2,
        predicted_winner: None,
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
    };
    assert_eq!(wf_a.acting_player(), Some(P1));
    assert_eq!(wf_b.acting_player(), Some(P2));

    // And RespondToShortcut routes to its `player`.
    let proposal = ShortcutProposal {
        proposer: P0,
        predicted_winner: Some(P0),
        count: IterationCount::UntilLethal,
        unbounded: vec![],
        win_kind: WinKind::LethalDamage,
        template: None,
    };
    let wf_r = WaitingFor::RespondToShortcut {
        player: P2,
        remaining_players: vec![],
        proposal,
    };
    assert_eq!(wf_r.acting_player(), Some(P2));

    // Turn-control sibling: P0 controls P1's turn, so it is the authorized transport
    // submitter for P1's priority-held offer even though P0 is also the predicted winner.
    // The proposal authority remains P1; only the player who submits P1's choice changes.
    let mut delegated = GameState::new_two_player(42);
    delegated.active_player = P1;
    delegated.priority_player = P0;
    delegated.turn_decision_controller = Some(P0);
    delegated.waiting_for = WaitingFor::LoopShortcut {
        proposer: P1,
        predicted_winner: Some(P0),
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
    };
    apply(&mut delegated, P0, GameAction::DeclineShortcut)
        .expect("the turn controller may submit the priority holder's decline");
    assert!(
        matches!(delegated.waiting_for, WaitingFor::Priority { player } if player == P1),
        "declining under turn control returns the semantic priority holder P1 to ordinary play"
    );
}

// ─────────────── T-concede-proposer (F1 revert-guard) ────────────────

/// The latched proposer P0 concedes DURING the open APNAP window. `Concede` bypasses the
/// `WaitingFor` dispatch (engine.rs), so `proposal.proposer` is never re-validated, and
/// because the acting player (P1) is still alive the elimination self-heal leaves the stale
/// offer standing. When the last opponent accepts, the proposer-liveness guard in
/// `apply_confirmed_shortcut` (F1) must REFUSE to crown the departed proposer — CR 104.3a (a
/// player who conceded has lost and cannot be crowned), CR 104.2a (the winner must still be
/// in the game), CR 800.4a (the proposer's loop objects have already left the game) — and
/// hand priority back instead. Reverting F1 makes P2's Accept crown
/// `GameOver{winner: Some(P0)}`, a departed winner, which this test forbids.
#[test]
fn interactive_proposer_concede_mid_apnap_does_not_crown_departed() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 proposes while it has priority");

    // P0 declares → APNAP window opens on P1, with P2 queued behind.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares");
    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "after Declare the APNAP window must open, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "window opens on P1");
    assert_eq!(remaining_players, vec![P2], "P2 queued behind P1");

    // The latched proposer P0 concedes MID-window (CR 104.3a: leaves + loses immediately).
    // The acting player is P1 (alive), so the elimination self-heal does NOT prune the
    // stale proposal — the window survives with a now-departed `proposal.proposer`.
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert!(is_eliminated(&runner, P0), "P0 has left the game");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "the offer survives the conceder (acting P1 is alive), got {:?}",
        runner.state().waiting_for
    );

    // P1 accepts → advance to P2 (still alive).
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "after P1 accepts, P2 (alive) is prompted, got {:?}",
        runner.state().waiting_for
    );

    // P2 accepts (last) → would crown the departed P0 if F1 were reverted.
    let last = runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P2 accepts (last)");

    // F1: the proposer-liveness guard refuses to crown the departed P0 and hands
    // priority back for a later LIVE re-detect. Reverting F1 flips this to
    // GameOver{winner: Some(P0)}.
    assert_ne!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "a departed proposer (P0 conceded) must NOT be crowned (CR 104.2a / 104.3a)"
    );
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => {
            assert!(
                !is_eliminated(&runner, player),
                "F1 must hand priority to a LIVING player (CR 800.4a), not the departed proposer; got Priority {{{player:?}}}"
            );
            assert_ne!(
                player, P0,
                "priority must not return to the conceded proposer P0"
            );
        }
        ref other => panic!("F1 hands priority back (manual fallback), got {other:?}"),
    }
    assert!(
        !last
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { winner } if *winner == Some(P0))),
        "no GameOver{{Some(P0)}} event may be emitted for the departed proposer"
    );
}

// ──────────────── T-concede-queued (F2 revert-guard) ────────────────

/// A QUEUED opponent (P2, not yet prompted) concedes AFTER the window opened. `Concede`
/// bypasses the `WaitingFor` dispatch, so `remaining_players` still lists the departed seat.
/// When the prompted opponent (P1) accepts, the liveness filter in
/// `handle_respond_to_shortcut` (F2) must DROP the departed seat and — finding no living
/// remainder — take the shortcut for the still-living proposer P0 instead of advancing onto
/// the departed P2 (CR 800.4a: never wait on a player who has left; F1 then re-validates P0's
/// own liveness before crowning). Reverting F2 makes P1's Accept set
/// `RespondToShortcut{player: P2}` — a permanent wait on a departed player.
#[test]
fn interactive_queued_opponent_concede_no_deadlock() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 proposes while it has priority");

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares");
    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "after Declare the APNAP window must open, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "window opens on P1");
    assert_eq!(remaining_players, vec![P2], "P2 queued behind P1");

    // The QUEUED (not-yet-prompted) opponent P2 concedes. Acting player is P1 (alive), so the
    // self-heal leaves the window on P1 — but `remaining_players` still lists the departed P2.
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(is_eliminated(&runner, P2), "P2 has left the game");
    assert!(
        !is_eliminated(&runner, P0) && !is_eliminated(&runner, P1),
        "P0/P1 remain in the game"
    );

    // P1 accepts. F2 drops departed P2 from the queue; no living remainder ⇒ take the
    // shortcut for the still-living P0 — NOT advance onto departed P2.
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts (last living opponent)");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "must NOT wait on the departed P2 (CR 800.4a), got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "the last living opponent accepted ⇒ crown the still-living proposer P0"
    );
}

// ───────────── T-subset-lethal (D2 — nonfallers.len()==1 guard) ─────────────

/// Drive cap for the three tests below. Measured beats actually consumed at this cap, per
/// drive: `setup_3p_subset_lethal` **24/24** (both tests), `setup_3p_both_fall(1000, 1050)`
/// **24/24**, `setup_3p_both_fall(1000, 1000)` **0/24**. The first three genuinely pay every
/// beat — that bridge deliberately falls through to the pre-feature grind, so their drives
/// never reach `drive_collect`'s terminal-state exit. The equal-life half is the exception and
/// costs nothing at ANY cap: its loop is mandatory with a single non-faller, so the natural
/// bridge already crowned `GameOver{Some(P0)}` while the kick-off resolved, and the drive
/// returns on beat 0. That is pre-existing and cap-independent — it is why shrinking this
/// constant speeds up three drives, not four.
///
/// Measured per-beat cost, `setup_3p_subset_lethal`: ~40 ms at invariant state (stack 1,
/// 4 objects). `setup_3p_both_fall` is NOT flat — its stack grows 13 → 45 and its per-beat
/// cost 78 → 152 ms over 200 beats — so its justification rests on verdict invariance, not on
/// periodicity.
///
/// MEASURED — DO NOT CHANGE to a value outside the swept set {4, 8, 12, 16, 24, 32, 48, 64,
/// 100} without re-running the cap sweep (an unswept *lowering* is as uncovered as a raise).
/// Across that whole swept range the PASS/FAIL verdict and every reach-guard of the three
/// tests below are invariant, and each test's revert-probe still flips it to FAIL at every
/// cap ≥ 4 (weakened `nonfallers.len() != 1`; bypassed E1-measure `live_mandatory_loop_winner`
/// gate; removed F2 `fallers_lives_pairwise_equal` re-check). Board recurrence — the regime
/// every assertion below assumes — is first observed at **beat 2** (measured over caps
/// {1, 2, 3, 4, 8, 24}), so more beats buy zero discrimination and only burn wall clock; 24 is
/// itself a swept value, 12× that priming point and 6× the smallest swept cap, not an
/// interpolation.
///
/// The cap does NOT stand on the sweep alone: each test below carries an explicit
/// [`drive_collect_primed`] guard that FAILS if the loop has not reached board recurrence inside
/// the cap — proven discriminating by forcing this constant to 1, which flips all three tests to
/// FAIL on that guard. That closes the cap-adequacy question, and only that.
///
/// It does NOT close a separate, pre-existing blind spot, and no cap does either. Measured:
/// suppressing the live-detect bridge outright (make its `!loop_detect_ring.is_empty()` conjunct
/// unreachable in engine.rs) leaves all three tests below GREEN at this cap AND at cap 500,
/// while the offer-dependent `vito_2p_optional_offer_declare_crowns` and
/// `interactive_queued_opponent_concede_no_deadlock` FAIL. The guard does not catch that either:
/// the ring SAMPLER is a separate gate (engine.rs, `resolved_this_beat && …`) that the
/// suppression does not touch, so the ring still fills and this guard still reports primed.
/// A negative "did not crown" test cannot distinguish a refused classification from a disabled
/// one; the positive-side tests named above are what cover it.
const PRIMED_LOOP_BEATS: usize = 24;

/// D2: a 3p loop that drains ONLY P1 (P2 a bystander, life delta 0) must NOT crown.
/// `live_mandatory_loop_winner` (loop_check.rs) partitions living into fallers/non-fallers and
/// requires `nonfallers.len() == 1` (CR 104.2a — determinate only when EVERY other living
/// player falls); here nonfallers = {P0, P2} (len 2) ⇒ `find_live_loop_winner` returns None,
/// so `interactive_loop_bridge` takes neither Path A (no determinate winner) nor Path B (a
/// life-loss axis is present, so not a CR 732.4 no-loss draw) and falls through to the
/// pre-feature grind.
///
/// REVERT-FAIL: weaken the `nonfallers.len() != 1` gate to an "any-faller wins" rewrite and
/// this MANDATORY loop is wrongly crowned `GameOver{winner: Some(P0)}` — flipping the `wf`
/// no-crown assertion below, which is the sole discriminator here: under that mutation the
/// event-scan assertion measurably stays TRUE (no `GameOver{Some}` lands in the collected
/// events) at every cap from 4 to 100. (Passes today, proving the gate holds.)
#[test]
fn interactive_3p_subset_lethal_does_not_crown() {
    let (mut runner, kickoff) = setup_3p_subset_lethal(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (events, wf, primed) = drive_collect_primed(&mut runner, PRIMED_LOOP_BEATS);

    // Positive reach-guard: the drain loop genuinely ran on P1 while P2 stayed untouched — we
    // are in the subset-lethal regime the gate must refuse, not an unrelated upstream no-op.
    assert!(
        life(&runner, P1) < 1000 && !is_eliminated(&runner, P1),
        "P1 must have bled (loop ran) but still be alive mid-drive, life = {}",
        life(&runner, P1)
    );
    assert_eq!(
        life(&runner, P2),
        20,
        "P2 is a bystander untouched by the loop (life delta 0 → a second non-faller)"
    );

    // No crown: a subset-lethal loop leaves >1 living non-faller, so no determinate winner.
    assert!(
        !matches!(wf, WaitingFor::GameOver { winner: Some(_) }),
        "subset-lethal loop must NOT crown a winner (CR 104.2a), got {wf:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { winner: Some(_) })),
        "no GameOver{{Some}} event may be emitted for a subset-lethal loop"
    );
    // No offer either: the bridge does not OFFER a shortcut for a non-winner loop.
    assert!(
        !matches!(wf, WaitingFor::LoopShortcut { .. }),
        "subset-lethal loop must NOT raise a LoopShortcut offer, got {wf:?}"
    );

    // Reach-guard on the regime itself (see `drive_collect_primed`): the drive really did reach
    // a board-recurrent state, so `live_mandatory_loop_winner` ran on the subset-lethal loop the
    // assertions above are about — they refused a real classification rather than never posing
    // one. Deliberately LAST here: this test's classification and its wrongful-crown failure
    // mode live in the same drive, so under the weakened-gate defect the crown must report as a
    // crown (the `wf` assertion above) — measured, a guard placed first steals that panic (M1
    // crowns at beat ~1, before recurrence) and reports the wrong cause.
    assert!(
        primed,
        "the loop never reached a board-recurrent state within {PRIMED_LOOP_BEATS} beats — this \
         is not the primed-loop regime the assertions above assume, so they passed vacuously"
    );
}

// ───────────── PR-7 Combo-UI Stage 2 — E1 drive-and-measure crown ──────────────
//
// The UntilLethal arm no longer crowns unconditionally: it DRIVES one pin-faithful cycle,
// MEASURES the per-cycle ResourceVector::delta, and re-runs `live_mandatory_loop_winner`
// (VERBATIM) — crowning ONLY when it names the proposer, else manual fallback. Plus the F2
// hardening (≥2-faller `fallers_lives_pairwise_equal` re-verification on the boundary).

/// 2p ESCALATING TARGETED drain (Vito+Sanguine+Bloodthirsty) made OPTIONAL by a castable
/// Lightning Bolt off an untapped Mountain on P1 (CR 732.5 probe FALSE ⇒ OFFER, not auto-win).
/// The forced-unique (single-opponent) targets auto-select at dispatch, so the only interactive
/// mid-drive prompt the E1 drive raises is OrderTriggers (the two simultaneous same-controller
/// drain triggers) — the template-independent injector arm.
fn setup_2p_vito_optional(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 4, VITO);
    scenario.add_creature_from_oracle(P0, "Sanguine Bond", 2, 2, SANGUINE_BOND);
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 3p DRAIN_CLERIC/BLOOD_SIPPER loop where BOTH opponents drain equally (CR 704.5a "each
/// opponent loses 1"). Configurable opponent life for the F2 ≥2-faller hardening tests: the
/// per-cycle delta is EQUAL for both (so `live_mandatory_loop_winner`'s ≥2-faller floor
/// passes), while the ABSOLUTE lives differ iff `p1_life != p2_life` (so the offer's own
/// `fallers_lives_pairwise_equal` distinguishes them). Started very high so the drive never
/// crosses lethal within one measured cycle (measure path, not cross-lethal).
fn setup_3p_both_fall(
    mode: LoopDetectionMode,
    p1_life: i32,
    p2_life: i32,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, p1_life);
    scenario.with_life(P2, p2_life);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// A synthetic `UntilLethal`/`LethalDamage` offer certificate for injecting a `LoopShortcut`
/// on a loop that never offers naturally (subset-lethal / >2p targeted).
fn synthetic_lethal_cert() -> LoopCertificate {
    LoopCertificate {
        unbounded: vec![],
        win_kind: WinKind::LethalDamage,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
    }
}

/// Accept the shortcut from every remaining living opponent (drain-one-advance APNAP), for
/// injected offers with any opponent count.
fn accept_all_opponents(runner: &mut GameRunner) {
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("living opponent accepts the shortcut");
    }
}

/// Test A ⭐ (END-TO-END, item 5 + item 4 OrderTriggers arm): the real 2p escalating targeted
/// drain OFFERS; P0 declares `UntilLethal` with NO template; on Accept the E1 drive re-fires
/// the loop, the injector answers the OrderTriggers prompt by identity order (the forced-unique
/// target auto-selects at dispatch), the cycle measures P1 as the sole faller, and
/// `live_mandatory_loop_winner` crowns P0. This is the end-to-end witness that the drive
/// traverses the trigger pipeline (OrderTriggers) to a crown — not a helper-level fallback.
#[test]
fn vito_2p_optional_offer_declare_crowns() {
    let (mut runner, kickoff) = setup_2p_vito_optional(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 2000);

    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional 2p Vito drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    // Reach-guard: the offer fired EARLY (P1 alive-positive), not at a natural death.
    assert!(
        life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
        "the offer must fire with P1 alive-positive, life = {}",
        life(&runner, P1)
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares UntilLethal (no template — forced-unique targets auto-select)");
    accept_all_opponents(&mut runner);

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "E1 drive-and-measure crowns P0 for the 2p targeted determinate drain (end-to-end \
         through the OrderTriggers injector arm)"
    );
}

/// Test B ⭐ (SOUNDNESS #1, item 5): a >2p SUBSET-lethal loop confirmed at APPLY does NOT crown
/// — the E1 drive measures ONE faller (P1) plus a second non-faller (P2, life-loss-immune), so
/// `live_mandatory_loop_winner` returns None (CR 104.2a) and the shortcut falls back to manual
/// play. REVERT-PROBE: making the crown unconditional (deleting the `live_mandatory_loop_winner`
/// gate) wrongly crowns P0 here.
#[test]
fn injected_3p_one_faller_no_crown() {
    let (mut runner, kickoff) = setup_3p_subset_lethal(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, _wf, primed) = drive_collect_primed(&mut runner, PRIMED_LOOP_BEATS);

    // Reach-guard on the regime (see `drive_collect_primed`): the board reached a genuine
    // recurrence, so the E1 clone-drive below has a real cycle to measure rather than an
    // un-primed board. (It witnesses the live bridge's frame pair, not the E1 measure's own
    // boundary/work pair — those are different frames.)
    assert!(
        primed,
        "the loop never reached a board-recurrent state within {PRIMED_LOOP_BEATS} beats — the \
         E1 measure below would have no primed cycle, so its no-crown assertion is vacuous"
    );

    // Reach-guard: the drain loop genuinely ran (P1 bled, alive) and P2 is untouched — this
    // is the subset-lethal regime the E1 measure must refuse.
    assert!(
        life(&runner, P1) < 1000 && !is_eliminated(&runner, P1),
        "P1 must have bled (loop primed), life = {}",
        life(&runner, P1)
    );
    assert_eq!(life(&runner, P2), 20, "P2 untouched (second non-faller)");

    // Inject the offer this subset-lethal loop never raises naturally, then confirm it.
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema: ShortcutDecisionSchema::default(),
    };
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares UntilLethal on the injected offer");
    accept_all_opponents(&mut runner);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(_) }
        ),
        "subset-lethal loop must NOT crown (CR 104.2a), got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the E1 measure hands back to manual play, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        life(&runner, P2),
        20,
        "the sim ran on a clone (state rolled back) — P2 still untouched"
    );
}

/// Test C ⭐ (LATENT FIX, item 5 object-growth branch): an object-growth ADVANTAGE token loop
/// declared `UntilLethal` (the AI hardcode shape) does NOT crown — the E1 object-growth branch
/// drives one recast, measures NO life/poison faller (only tokens grew), so
/// `live_mandatory_loop_winner` returns None and the shortcut falls back to manual play.
/// REVERT-PROBE: the pre-E1 unconditional UntilLethal crown wrongly ends the game here.
#[test]
fn object_growth_advantage_untillethal_no_crown() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let before = saproling_count(runner.state());
    let _ = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { proposer, predicted_winner, .. } if proposer == P0 && predicted_winner.is_none()),
        "the object-growth cast must OFFER a LoopShortcut to P0, got {:?}",
        runner.state().waiting_for
    );
    // Reach-guard: the real cast grew the board by one Saproling (the recast ran) — we are on
    // the object-growth branch, not an unrelated no-op.
    assert!(
        saproling_count(runner.state()) > before,
        "the real cast must have grown the board (object-growth branch reachable)"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares UntilLethal on the Advantage offer (AI-hardcode shape)");
    accept_all_opponents(&mut runner);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "an inert Advantage token loop must NOT crown under UntilLethal, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the E1 object-growth measure hands back to manual play, got {:?}",
        runner.state().waiting_for
    );
    for p in [P0, P1] {
        assert!(
            life(&runner, p) > 0,
            "no player crossed lethal (no drain axis)"
        );
    }
}

/// Test E ⭐ (SOUNDNESS #1 firewall, item 3): the declare-time `validate_pins` firewall REJECTS
/// an illegal-value pin (a target outside the slot's offered `legal_targets`) BEFORE APNAP opens
/// (⇒ manual-play Priority), and INGESTS a legal pin (⇒ RespondToShortcut opens). REVERT-PROBE:
/// removing the validate hook lets the illegal pin open the response window (a leak).
#[test]
fn declare_illegal_pin_falls_back_legal_ingests() {
    // A schema exposing ONE Targets slot whose only legal target is Player(P1).
    let source = YieldTarget::ThisObject {
        source_id: ObjectId(1),
        incarnation: None,
        trigger_description: None,
    };
    let slot = DecisionSlot {
        source: source.clone(),
        index: 0,
    };
    let schema = ShortcutDecisionSchema {
        iteration_count: IterationCount::UntilLethal,
        // No narrowed CR 732.2a bound — the global cap, as every offer states today.
        max_iterations: ShortcutDecisionSchema::default().max_iterations,
        points: vec![DecisionPoint {
            slot: slot.clone(),
            kind: DecisionPointKind::Targets {
                legal_targets: vec![TargetRef::Player(P1)],
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };
    let template_for = |pinned: PlayerId| DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets {
            slot: slot.clone(),
            targets: vec![TargetPin::Player(pinned)],
        }],
        replay: ReplayMode::Scheduled {
            count: IterationCount::UntilLethal,
        },
        key: DecisionGroupKey::from_sources(
            std::slice::from_ref(&source),
            DecisionKind::LoopChoice,
        ),
    };

    // ILLEGAL half: pin Player(P2), not in the offered legal set ⇒ rejected to Priority.
    let (mut runner, _kickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema: schema.clone(),
    };
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: Some(template_for(P2)),
        })
        .expect("declare dispatch succeeds (the rejection is a manual-fallback, not an error)");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "an illegal-value pin is REJECTED before APNAP (manual fallback), got {:?}",
        runner.state().waiting_for
    );

    // LEGAL half (reach-guard, not always-reject): pin Player(P1) ⇒ RespondToShortcut opens.
    let (mut runner2, _kickoff2) = setup_3p_draw(LoopDetectionMode::Interactive);
    runner2.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema,
    };
    runner2
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: Some(template_for(P1)),
        })
        .expect("declare with a legal pin");
    assert!(
        matches!(
            runner2.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "a legal pin is INGESTED — the response window opens, got {:?}",
        runner2.state().waiting_for
    );
}

/// Test G ⭐ (F2 HARDENING, item 5 ≥2-faller re-verification): a >2p drain that drops TWO
/// opponents by EQUAL per-cycle deltas but at UNEQUAL absolute life does NOT crown — the
/// ≥2-faller `fallers_lives_pairwise_equal` re-check on the pre-drive boundary fails
/// (staggered CR 704.3 lethal). The EQUAL-life sibling DOES crown (reach-guard proving the
/// check is not always-reject). REVERT-PROBE: removing the F2 check wrongly crowns the
/// unequal-life half.
#[test]
fn injected_3p_unequal_life_pin_all_no_crown() {
    // Drive one primed cycle of a confirmed 3p both-fall drain and report the terminal
    // waiting_for.
    fn drive_confirmed(p1_life: i32, p2_life: i32) -> (WaitingFor, bool) {
        let (mut runner, kickoff) =
            setup_3p_both_fall(LoopDetectionMode::Interactive, p1_life, p2_life);
        let _ = runner.cast(kickoff).resolve();
        let (_events, _wf, primed) = drive_collect_primed(&mut runner, PRIMED_LOOP_BEATS);
        // Reach-guard: both opponents bled equally (loop primed, both are fallers) and stay
        // pairwise-offset by the initial gap (equal deltas preserve the difference).
        assert!(
            life(&runner, P1) < p1_life && life(&runner, P2) < p2_life,
            "both opponents must have bled (≥2-faller regime primed)"
        );
        assert_eq!(
            p2_life - p1_life,
            life(&runner, P2) - life(&runner, P1),
            "equal per-cycle deltas preserve the pairwise life gap"
        );
        runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner: Some(P0),
            certificate: synthetic_lethal_cert(),
            schema: ShortcutDecisionSchema::default(),
        };
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::UntilLethal,
                template: None,
            })
            .expect("P0 declares UntilLethal");
        accept_all_opponents(&mut runner);
        (runner.state().waiting_for.clone(), primed)
    }

    // UNEQUAL absolute life (gap 50) ⇒ NO crown (F2 staggered-death veto).
    let (unequal, unequal_primed) = drive_confirmed(1000, 1050);
    // Reach-guard on the regime (see `drive_collect_primed`) — asserted on THIS half only: the
    // equal-life half below is measured to consume 0 beats (already crowned as the kick-off
    // resolved), so it has no drive in which to recur. This is the half the F2 revert-probe
    // flips, so the whole discriminator lives on the guarded side.
    assert!(
        unequal_primed,
        "the loop never reached a board-recurrent state within {PRIMED_LOOP_BEATS} beats — this \
         is not the ≥2-faller primed regime the assertions below assume"
    );
    assert!(
        !matches!(unequal, WaitingFor::GameOver { winner: Some(_) }),
        "unequal-life ≥2-faller drain must NOT crown (CR 704.3 simultaneity), got {unequal:?}"
    );
    assert!(
        matches!(unequal, WaitingFor::Priority { .. }),
        "the F2 veto hands back to manual play, got {unequal:?}"
    );

    // EQUAL absolute life ⇒ CROWN (reach-guard: the F2 check is not always-reject).
    let (equal, _) = drive_confirmed(1000, 1000);
    assert_eq!(
        equal,
        WaitingFor::GameOver { winner: Some(P0) },
        "equal-life ≥2-faller drain still crowns P0 (F2 pairwise-equal passes)"
    );
}

// ─────────────────── T-B3-materialize (Phase 4b) ───────────────────────

/// Reach `LoopShortcut{P0}` on a fresh `setup_2p_optional_drain(Interactive)` fixture.
/// Returns the runner parked at the offer, `life(P1)` at that instant, and the
/// DRAIN_CLERIC object id (for template pins).
fn reach_2p_optional_drain_offer() -> (GameRunner, i32, ObjectId) {
    let (mut runner, kickoff, _bolt, cleric) =
        setup_2p_optional_drain(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    let l0 = life(&runner, P1);
    (runner, l0, cleric)
}

/// Probe the per-cycle P1 drain constant via an independent `Fixed(1)` materialization
/// of the DRAIN_CLERIC/BLOOD_SIPPER pairing (one recurrence = one full cycle).
fn probe_drain_delta() -> i32 {
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare Fixed(1)");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    let delta = l0 - life(&runner, P1);
    assert!(
        delta > 0,
        "Fixed(1) must materialize a nonzero drain cycle, got delta={delta}"
    );
    delta
}

/// A `Fixed(count)` template pinning `object` by `ThisObject{incarnation}` — CR 400.7's
/// per-iteration incarnation re-bind (BLOCKER #4 real teeth).
fn incarnation_pin_template(
    owner: PlayerId,
    object: ObjectId,
    incarnation: u64,
    count: IterationCount,
) -> DecisionTemplate {
    let source = YieldTarget::ThisObject {
        source_id: object,
        incarnation: Some(incarnation),
        trigger_description: None,
    };
    let slot = DecisionSlot {
        source: source.clone(),
        index: 0,
    };
    DecisionTemplate {
        owner,
        decisions: vec![PinnedDecision::Targets {
            slot,
            targets: vec![TargetPin::ByIdentity(source.clone())],
        }],
        replay: ReplayMode::Scheduled { count },
        key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
    }
}

/// A `Fixed(count)` template pinning `cleric` via a PRE-DECLARED (CR 732.2a-predictable)
/// `Piecewise` schedule: iterations `[0, switch)` resolve to `cleric` itself (stable
/// across the drive); at `switch` (if `Some`) the schedule switches to a bogus,
/// never-resolvable `ObjectId` — simulating "the pinned object left the game" at exactly
/// that iteration, entirely from the schedule (no mid-drive test backdoor).
fn piecewise_cleric_template(
    owner: PlayerId,
    cleric: ObjectId,
    switch_to_bogus_at: Option<u32>,
    count: IterationCount,
) -> DecisionTemplate {
    let valid = YieldTarget::ThisObject {
        source_id: cleric,
        incarnation: None,
        trigger_description: None,
    };
    let bogus = YieldTarget::ThisObject {
        source_id: ObjectId(u64::MAX),
        incarnation: None,
        trigger_description: None,
    };
    let slot = DecisionSlot {
        source: valid.clone(),
        index: 0,
    };
    let mut schedule = vec![(0u32, valid.clone())];
    if let Some(at) = switch_to_bogus_at {
        schedule.push((at, bogus));
    }
    DecisionTemplate {
        owner,
        decisions: vec![PinnedDecision::Targets {
            slot,
            targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(schedule))],
        }],
        replay: ReplayMode::Scheduled { count },
        key: DecisionGroupKey::from_sources(&[valid], DecisionKind::LoopChoice),
    }
}

/// B3-materialize-stop-short ⭐ (N < cycles-to-lethal): P1's life must drop EXACTLY
/// `N*delta` — a NON-ZERO multiple. This is the empirical BLOCKER #2 gate: if the
/// per-cycle recurrence boundary is unseeded (`waiting_for` never re-matches
/// `Priority{active}`), the drive spins to `cycle_beat_cap` every iteration and aborts at
/// 0 complete cycles, so drop==0 and this assertion FAILS; under the pre-4b decline-stub,
/// drop==0 too — both revert targets are caught by the same assertion.
#[test]
fn b3_materialize_stop_short() {
    let delta = probe_drain_delta();
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();
    let n: u32 = 3;
    assert!(
        (n as i32) * delta < l0,
        "test precondition: N*delta must stay short of lethal (l0={l0}, delta={delta})"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        })
        .expect("declare Fixed(N)");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");

    assert_eq!(
        life(&runner, P1),
        l0 - (n as i32) * delta,
        "P1 life must drop EXACTLY N*delta"
    );
    assert!(
        !is_eliminated(&runner, P1),
        "P1 must remain alive (N below cycles-to-lethal)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "must not reach GameOver, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "materialization stops at Priority{{living_priority_seat}} (P0) — manual fallback, \
         not a wrong-crown or a stuck handback"
    );
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the ring must be cleared on stop-short (Q3) so the same apply() does not instantly \
         re-offer"
    );
}

/// PR-7 DoS cap (CR 732.2a SAFETY LIMIT): a `Fixed` count over `MAX_SHORTCUT_CYCLES` is
/// handed back to manual play with NO drive. This is the engine-side count cap that stops the
/// catastrophic 4-byte remote vector — `Fixed(u32)` scalar-encodes ~4.3e9 cycles in ~10 bytes,
/// sailing through the 8 KB WS frame cap. The count is HARDCODED as `Fixed(u32::MAX)`; the cap
/// const is private to the engine crate and invisible across this integration-test boundary.
///
/// VACUITY TRAP (PR-7): a handback lands on `WaitingFor::Priority`, and so does the cap-ABSENT
/// stop-short path (a drive commits + stops there too). So `waiting_for` alone is an INVARIANT,
/// not a discriminator. The DISCRIMINATOR is the observable DRIVE: `life(P1) == l0` proves the
/// cap fired before any cycle ran. The revert-probe (delete Edit B's guard body) opens APNAP,
/// Accept drives, and on this life-DRAIN fixture P1 crosses lethal in ~l0/delta cycles (tens —
/// `materialize_fixed_shortcut`'s CrossLethal arm commits + stops, so `u32::MAX` does NOT hang)
/// ⇒ `life(P1) ≤ 0` + GameOver ⇒ `assert_eq!(life(P1), l0)` FAILS.
///
/// Positive reach-guard: `b3_materialize_stop_short` (n=3) proves the harness DOES drive when
/// n ≤ cap, so T1's no-drive is the cap firing, not a dead fixture.
#[test]
fn over_cap_fixed_count_hands_back_with_no_drive() {
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(u32::MAX),
            template: None,
        })
        .expect("declare Fixed(u32::MAX)");

    // Symmetric-across-revert Accept: with Edit B active the declare hands back immediately
    // (APNAP never opens), so the Accept would be illegal — issue it ONLY when actually parked
    // at RespondToShortcut (the cap-absent revert path).
    if matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accept");
    }

    // DRIVE discriminator: the cap fired BEFORE any cycle ran, so P1's life is untouched.
    assert_eq!(
        life(&runner, P1),
        l0,
        "over-cap Fixed hands back with NO drive — P1 life unchanged (the discriminator)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "no crown — the drive never ran, got {:?}",
        runner.state().waiting_for
    );
    // SANITY CHECK ONLY (not the discriminator — see the vacuity trap in the doc): the handback
    // lands on the living seat, mirroring the stop-short manual fallback.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "handback to living_priority_seat (P0)"
    );
}

/// B3-materialize-cross-lethal ⭐ (N ≥ cycles-to-lethal, un-clamped per Q2): commits and
/// stops at a determinate GameOver mid-drive instead of rolling back to manual play.
/// Revert-failing / discriminating vs stop-short: under a flat "non-Priority ⇒ rollback"
/// reducer (the pre-4b decline-stub, or a naive unconditional-abort materializer), this
/// reverts to manual play — P1 SURVIVES at positive life and `waiting_for == Priority` —
/// flipping every assertion below. The stop-short/cross-lethal PAIR (same fixture, N
/// below vs comfortably above cycles-to-lethal) is the discriminator.
#[test]
fn b3_materialize_cross_lethal() {
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();
    // Un-clamped (Q2): N is comfortably past any plausible per-cycle delta >= 1, so this
    // exercises N far beyond cycles-to-lethal without needing the exact probed delta.
    let n: u32 = (l0 as u32) * 2 + 10;
    let unbounded_before = runner.state().unbounded_resources.clone();

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        })
        .expect("declare Fixed(N)");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "N >= cycles-to-lethal must COMMIT + STOP at a determinate GameOver mid-drive"
    );
    assert!(
        life(&runner, P1) <= 0 && is_eliminated(&runner, P1),
        "P1 must be dead (drained to <=0), NOT rolled back to positive life"
    );
    assert_eq!(
        runner.state().unbounded_resources,
        unbounded_before,
        "a finite Fixed(N) drain must NOT mark_unbounded_loop (finite != unbounded, contrast \
         the UntilLethal arm)"
    );
}

/// B3-firewall-abort (BLOCKER #4 real teeth, hostile): `resolve()`'s CR 400.7 incarnation
/// re-bind is the load-bearing per-iteration firewall — `predictability_gate(t, &[])` is a
/// wired FORMAL no-op this phase (empty `required_slots`; its own discriminating coverage
/// is the pre-existing `decision_template.rs` unit tests, not re-claimed here).
/// Positive/negative pair on the SAME template pinning DRAIN_CLERIC by
/// `ThisObject{incarnation}`: incarnation stable ⇒ N cycles materialize; incarnation
/// bumped (simulating a leave+re-entry) BEFORE the drive starts ⇒ `resolve` fails on
/// iteration 0 ⇒ abort at 0 complete cycles, priority handback, loop broken.
#[test]
fn b3_firewall_abort_incarnation_guard() {
    let delta = probe_drain_delta();
    let n: u32 = 3;

    // Positive: incarnation stable across the whole drive.
    let (mut runner, l0, cleric) = reach_2p_optional_drain_offer();
    let inc = runner
        .state()
        .objects
        .get(&cleric)
        .expect("cleric on battlefield")
        .incarnation;
    let template = incarnation_pin_template(P0, cleric, inc, IterationCount::Fixed(n));
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(template),
        })
        .expect("declare");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        life(&runner, P1),
        l0 - (n as i32) * delta,
        "stable incarnation ⇒ resolve() succeeds every iteration ⇒ all N cycles materialize"
    );
    assert!(!is_eliminated(&runner, P1));

    // Negative (hostile): bump the pinned object's incarnation AFTER Declare but BEFORE
    // Accept — simulating a leave+re-entry inside the still-open window — while the
    // template still carries the STALE incarnation it was pinned with.
    let (mut runner2, l0b, cleric2) = reach_2p_optional_drain_offer();
    let inc2 = runner2
        .state()
        .objects
        .get(&cleric2)
        .expect("cleric on battlefield")
        .incarnation;
    let template2 = incarnation_pin_template(P0, cleric2, inc2, IterationCount::Fixed(n));
    runner2
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(template2),
        })
        .expect("declare");
    runner2
        .state_mut()
        .objects
        .get_mut(&cleric2)
        .expect("cleric on battlefield")
        .incarnation += 1;
    runner2
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");

    assert_eq!(
        life(&runner2, P1),
        l0b,
        "stale-incarnation resolve() failure must abort at 0 complete cycles (no drain leaked)"
    );
    assert!(!is_eliminated(&runner2, P1));
    assert_eq!(
        runner2.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "abort hands priority back to living_priority_seat (P0), not a wrong-crown"
    );
    assert!(runner2.state().loop_detect_ring.is_empty());
}

/// B3-abort-rollback-live (CR 608.2b + atomicity): a PRE-DECLARED `Piecewise` schedule
/// pins DRAIN_CLERIC for cycles `[0, k)` then switches to a never-resolvable object at
/// cycle `k` — simulating "the enabler leaves the game" exactly at the k-th iteration,
/// entirely from the schedule (no mid-drive test backdoor). Asserts the drained life is
/// an EXACT multiple `k*delta` — no partial-cycle leak: the aborting iteration k's `ev`
/// must have been dropped, not merged. Negative pair: the SAME schedule shape with the
/// switch point placed past N materializes all N cycles untouched.
#[test]
fn b3_abort_rollback_live_atomicity() {
    let delta = probe_drain_delta();
    let n: u32 = 8;
    let k: u32 = 3;
    assert!(
        k < n,
        "test setup: abort must land strictly before N completes"
    );

    // Negative pair: switch point past N ⇒ no removal ⇒ all N cycles commit.
    let (mut clean_runner, l0_clean, cleric_clean) = reach_2p_optional_drain_offer();
    let clean_template =
        piecewise_cleric_template(P0, cleric_clean, Some(n + 100), IterationCount::Fixed(n));
    clean_runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(clean_template),
        })
        .expect("declare");
    clean_runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        life(&clean_runner, P1),
        l0_clean - (n as i32) * delta,
        "no removal ⇒ all N cycles commit"
    );

    // Positive (hostile): switch point AT k ⇒ cycles [0,k) commit, cycle k aborts.
    let (mut runner, l0, cleric) = reach_2p_optional_drain_offer();
    let template = piecewise_cleric_template(P0, cleric, Some(k), IterationCount::Fixed(n));
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(template),
        })
        .expect("declare");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");

    assert_eq!(
        life(&runner, P1),
        l0 - (k as i32) * delta,
        "rollback must land at EXACTLY k complete cycles — no partial (aborting) cycle leaked"
    );
    assert!(!is_eliminated(&runner, P1));
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "abort hands priority back to living_priority_seat (P0)"
    );
    assert!(runner.state().loop_detect_ring.is_empty());
}

// ═══════════════════ PR-7 Phase 4c — B5 revocable-∞ + LOW-2 ═══════════════════

/// Poison rider for the DRAW-gate behavioral test: fires on the SAME "whenever you gain
/// life" event the SELF_LIFE_ENGINE cascade pumps, dripping a poison counter onto each
/// opponent every cycle. Non-targeted (no mid-drive target prompt ⇒ mandatory-preserving).
const POISON_RIDER: &str = "Whenever you gain life, each opponent gets a poison counter.";

/// 3-player MANDATORY self-sustaining lifegain cascade (SELF_LIFE_ENGINE) that ALSO drips
/// poison onto each opponent every cycle (POISON_RIDER, a SEPARATE second trigger). Nobody
/// loses LIFE (so Path A's `live_mandatory_loop_winner` finds no faller ⇒ nonfallers≠1 ⇒
/// None); opponents accrue POISON.
///
/// MEASURED reachability (this 2-trigger fixture does NOT reach the Path-B bridge): the two
/// simultaneous triggers per lifegain event open OrderTriggers beats, and every non-
/// `Priority{active_player}` beat CLEARS `loop_detect_ring` (engine.rs:1307). So the ring
/// never accumulates, the `!ring.is_empty()` bridge gate (engine.rs:338) never passes, and
/// `interactive_loop_bridge` is never entered (measured: 0 bridge invocations). The loop
/// instead resolves via the CR 704.5c 10-poison SBA to GameOver{Some(P0)} (both opponents
/// reach 10 poison and are eliminated). It therefore does NOT exercise the Path-B
/// `has_no_loss_axis` veto — see the test doc below.
fn setup_3p_poison_draw(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
    scenario.add_creature_from_oracle(P0, "Test Poison Dripper", 2, 2, POISON_RIDER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// Path-B DRAW-GATE behavioral test (two halves):
///   - CONTROL (`setup_3p_draw`, pure lifegain, no poison) is a POSITIVE test that the Path-B
///     draw gate CERTIFIES a benign no-loss loop: it draws `GameOver{None}` via engine.rs:517
///     (measured P0 life 22, cycle ~2; and neutering :517 makes this control STOP drawing —
///     confirmed the draw originates AT that gate, not the strict :1507 detector).
///   - VARIANT (`setup_3p_poison_draw`, IDENTICAL + a poison-rider creature) locks that a
///     poison-accruing loop is NOT wrongly drawn: it resolves via the CR 704.5c 10-poison SBA
///     to `GameOver{Some(P0)}` (measured P0 life 30, poisons [0,10,10], both opponents
///     eliminated).
///
/// SCOPE (measured — do NOT overclaim): this does NOT isolate `has_no_loss_axis`'s Path-B
/// conjunct. That conjunct IS load-bearing BY CONSTRUCTION (it is the SOLE loss-axis veto at
/// :512-516, which has NO `== Advantage` backstop — a poison loop that reached the gate would
/// be wrongly drawn without it), but it is currently NOT runtime-discriminable, so there is NO
/// claim here that deleting it flips the variant. MEASURED: deleting `has_no_loss_axis` from
/// Path B leaves the variant terminal `GameOver{Some(P0)}` UNCHANGED — because the variant
/// never REACHES the gate with poison>0. A single-compound-trigger poison loop DOES reach the
/// Path-B bridge, but the "you gain N life and [each opponent gets a poison counter]" parser
/// drop removes the poison conjunct (card-build keeps only `GainLife`), so poison is 0 in the
/// loop delta at the gate → it draws as a benign lifegain loop and never exercises
/// has_no_loss_axis's poison veto. No constructible fixture carries poison>0 to the Path-B gate
/// (the 2-trigger form clears `loop_detect_ring` on its OrderTriggers beats at engine.rs:1307;
/// the single-compound-trigger form drops the poison at parse). So the Path-B veto is proven
/// load-bearing IN CODE and its runtime discriminator is WAIVED pending the poison-drop parser
/// fix.
///
/// POST-RE-KEY NOTE (PR-7 poison pass): `has_no_loss_axis`'s poison veto now reads the
/// per-victim `delta.poison` map (was the aggregate `delta.counters[(Poison, Player)]`).
/// The veto FIELD moved; the Path-B reachability did NOT — this test is unchanged and stays
/// the SBA-terminal behavioral anchor.
#[test]
fn interactive_recurring_poison_is_not_drawn() {
    // CONTROL (differential anchor): the SHARED pure-lifegain structure reaches the CR 732.4
    // gate and DRAWS — establishes that this fixture shape is one that CAN be certified a draw,
    // so the variant's not-drawing is attributable to the one added line (the poison rider).
    let (mut control, ckickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    let _ = control.cast(ckickoff).resolve();
    let (_ce, cwf) = drive_collect(&mut control, 500);
    assert_eq!(
        cwf,
        WaitingFor::GameOver { winner: None },
        "control anchor: the pure-lifegain structure IS certified a CR 732.4 draw — so the ONLY \
         fixture change (the poison rider) is what makes the variant below not-draw"
    );

    // VARIANT: identical structure + exactly one poison-rider creature (the single-line delta).
    let (mut runner, kickoff) = setup_3p_poison_draw(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (events, wf) = drive_collect(&mut runner, 500);

    // Positive reach-guard (non-vacuity): the poison LOSS axis was genuinely driven to its
    // CR 704.5c terminal — BOTH opponents reached ≥10 poison and were eliminated. Without this,
    // "not drawn" could hold trivially (the loop never ran / poison never applied).
    let poisons: Vec<u32> = runner
        .state()
        .players
        .iter()
        .map(|p| p.poison_counters)
        .collect();
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .filter(|p| p.is_eliminated && p.poison_counters >= 10)
            .count(),
        2,
        "reach-guard: both opponents must be poisoned out (CR 704.5c, ≥10 poison + eliminated), \
         proving the loss axis genuinely drove a determinate loss; got poisons {poisons:?}"
    );

    // The guard: the poison loop must NOT be a CR 732.4 draw, and must resolve to the correct
    // determinate CR 704.5c poison loss (P0 the sole survivor).
    assert_ne!(
        wf,
        WaitingFor::GameOver { winner: None },
        "recurring poison loop must NOT be certified a CR 732.4 draw; got {wf:?}"
    );
    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "the poison loop resolves to P0's determinate win (both opponents poisoned out), not a draw"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { winner: None })),
        "no CR 732.4 draw event may be emitted for a poison-dripping loop"
    );
}

/// Single-trigger drain that ALSO drips poison onto each opponent — the compound
/// "loses 1 life AND gets a poison counter" survives the parser as BOTH conjuncts
/// (measured), so the per-cycle delta carries `poison[opp] = +1` alongside `life[opp] = -1`.
const DRAIN_POISON_CLERIC: &str =
    "Whenever you gain life, each opponent loses 1 life and gets a poison counter.";

/// 2-player OPTIONAL self-refilling drain-that-also-poisons controlled by P0. The pairing of
/// `DRAIN_POISON_CLERIC` with `BLOOD_SIPPER` forms the proven ring-accumulating
/// single-trigger-per-event ping-pong (`setup_2p_optional_drain` shape); the compound adds a
/// poison counter to each opponent each cycle. P1 holds a castable Bolt off a Mountain ⇒ the
/// loop is OPTIONAL ⇒ Path A OFFERS.
fn setup_2p_optional_drain_poison(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Poison Cleric", 2, 2, DRAIN_POISON_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// PR-7 poison-axis E2E: the re-keyed per-victim `ResourceAxis::Poison(PlayerId)` surfaces in a
/// REAL offer certificate, produced end-to-end through the live sampler → `interactive_loop_bridge`
/// (Path A) → `find_live_loop_winner` → `build_cert` → `unbounded_axes_for`. This is the
/// production-path proof that the Option-A re-key (G-5 `unbounded_components` / G-6 the enum
/// variant) flows into a live certificate, not just a hand-built `mark_unbounded_loop` (T5).
///
/// SCOPE (measured — do NOT overclaim): the loop's DECIDING win_kind here is `LethalDamage`
/// (CR 704.5a life drain — classify checks opponent-life-loss before poison), so this is NOT the
/// `win_kind == PoisonLoss` full-drive witness. That witness is WAIVED (§6 rung-3): NO
/// single-compound-trigger poison-DECIDING loop can drive the live sampler —
///   • the self-refilling PROLIFERATE form (`"...you gain 1 life, then proliferate."`) opens a
///     `ProliferateChoice` beat every cycle, which is neither `Priority{active}` nor
///     `OrderTriggers` ⇒ it hits the sampler CLEAR arm (engine.rs `record_loop_detect_sample`
///     gate), so the ring never accumulates a recurrence and the loop reaches the natural
///     CR 704.5c 10-poison SBA instead of offering (MEASURED: 0 offers, natural GameOver);
///   • the `"you gain N life and each opponent gets a poison counter"` compound DROPS the poison
///     conjunct at parse (keeps only `GainLife`), so poison never reaches the delta.
/// Both are pre-existing sampler/parser limitations, independent of this change (see
/// `interactive_recurring_poison_is_not_drawn` above, loop_shortcut.rs:1191-1239). The novel
/// per-victim classify/faller logic is proven by the `loop_check.rs` unit tests
/// (`live_winner_names_poison_faller`, `detects_poison_loop_as_poison_loss`, the refuse cases);
/// this test adds the missing END-TO-END proof that the re-keyed axis reaches a live cert.
///
/// TWO-PATH ARCHITECTURE (why this is a boundary, not scope-shrink): the real Kilo/Freed/Relic
/// activation combo IS covered — by the OFFLINE certification driver `drive_offline_kilo_freed_relic`
/// (`analysis/corpus.rs` DRIVERS row 1), the same path the PR-7 combo-declaration UI feeds. The
/// live equality-sampler cannot see an activation loop BY CONSTRUCTION (a player-driven activation
/// drains the stack between activations → the `record_loop_detect_sample` CLEAR arm fires →
/// `loop_detect_ring` never accumulates → the bridge gate `!ring.is_empty()` never passes). So the
/// two detection paths partition cleanly: offline/declared certification → activation & pinned
/// loops; the live sampler → self-refilling trigger cascades. This test exercises the live G1
/// poison-cert path with the self-refilling drain trigger — the shape the sampler actually detects.
/// // ponytail: activation/proliferate loops aren't live-sampled (stack drains / ring clears);
/// // the self-refilling drain trigger IS the detectable shape — it carries the poison axis into
/// // the offer cert even though life is the deciding clock.
///
/// DISCRIMINATOR / revert-probe: revert G-5 (drop the `for (pid, &n) in &self.poison` push in
/// `unbounded_components`) ⇒ after G-2 moved poison out of `.counters`, the cert would carry
/// NEITHER `Poison(P1)` NOR `Counter(Poison, Player)` ⇒ assertion (3) flips to fail.
#[test]
fn interactive_poison_axis_surfaces_in_offer_certificate() {
    let (mut runner, kickoff) = setup_2p_optional_drain_poison(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    // (1) Path A OFFERED (not an auto-win): P0 has priority and is the predicted winner.
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        ..
    } = wf.clone()
    else {
        panic!("optional drain-poison loop must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    assert_eq!(predicted_winner, Some(P0), "the detector predicts P0 wins");

    // (2) Positive reach-guard (non-vacuity): the offer fired EARLY (not the natural CR 704.5c
    // 10-poison SBA), and the poison axis genuinely MOVED — P1 bears >=1 poison but <10 and is
    // still alive at positive life. Without this, "cert carries Poison" could hold on a
    // degenerate cert where poison never actually accrued.
    assert!(
        !is_eliminated(&runner, P1) && life(&runner, P1) > 0,
        "reach-guard: P1 must be alive at positive life when the offer fires (early, not natural death)"
    );
    let p1_poison = runner.state().players[1].poison_counters;
    assert!(
        (1..10).contains(&p1_poison),
        "reach-guard: P1 must bear 1..10 poison at offer time (the loss axis genuinely moved); got {p1_poison}"
    );

    // (3) THE DISCRIMINATOR: the re-keyed per-victim poison axis is carried in a REAL cert.
    assert!(
        certificate.unbounded.contains(&ResourceAxis::Poison(P1)),
        "the offer certificate must carry the re-keyed Poison(P1) axis; got {:?}",
        certificate.unbounded
    );

    // (4) The offer resolves: proposer declares, the sole opponent accepts ⇒ GameOver{P0}.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares the shortcut");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts (sole opponent) → take the shortcut");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "accepted ⇒ the shortcut resolves to P0's win"
    );
}

/// Drive PassPriority/OrderTriggers beats like `drive_collect`, but stop as soon as
/// `stop` is satisfied rather than waiting for a non-Priority/OrderTriggers terminal
/// state. Path C (B5) is a SILENT mark — it never changes `waiting_for` — so
/// `drive_collect`'s stop condition never fires for it; callers that need to observe a
/// mid-grind fact (the mark landing, a specific player's priority window) poll state
/// directly each beat instead.
fn drive_until(
    runner: &mut GameRunner,
    cap: usize,
    mut stop: impl FnMut(&GameState) -> bool,
) -> bool {
    for _ in 0..cap {
        if stop(runner.state()) {
            return true;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return false;
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err()
                    && runner
                        .act(GameAction::OrderTriggers { order: vec![] })
                        .is_err()
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    stop(runner.state())
}

/// Stop as soon as `controller`'s revocable-∞ capability is marked.
fn drive_until_marked(runner: &mut GameRunner, controller: PlayerId, cap: usize) -> bool {
    drive_until(runner, cap, |s| {
        s.unbounded_resources.contains_key(&controller)
    })
}

/// Stop as soon as `player` holds a live priority window (used to reach a specific
/// player's priority inside a self-sustaining loop, where a plain drive just alternates
/// between players indefinitely).
fn advance_to_player_priority(runner: &mut GameRunner, player: PlayerId, cap: usize) -> bool {
    drive_until(
        runner,
        cap,
        |s| matches!(s.waiting_for, WaitingFor::Priority { player: p } if p == player),
    )
}

/// 2-player OPTIONAL beneficial (self-lifegain) loop controlled by P0 — the live B5
/// producer class (R4: triggered-ability beneficial cascades). No faller (Path A finds no
/// winner: `find_live_loop_winner` requires an opponent life-faller). P1 holds a castable
/// Bolt off an untapped Mountain (a meaningful priority action) so the loop is OPTIONAL
/// (`mandatory == false`); the Bolt targets the life-engine creature for B5-2's defuse.
/// Returns runner + (kickoff, bolt, life-engine creature id).
fn setup_2p_optional_beneficial(
    mode: LoopDetectionMode,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let engine_creature = scenario
        .add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE)
        .id();
    scenario.add_basic_land(P1, ManaColor::Red);
    let bolt = scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff, bolt, engine_creature)
}

/// B5-1 (positive): an OPTIONAL beneficial loop under `Interactive` is neither crowned
/// (Path A: no faller) nor drawn (Path B: `!mandatory`) — it is marked as a revocable-∞
/// capability (Path C) and the game continues at live priority.
#[test]
fn b5_optional_beneficial_marks_revocable_unbounded() {
    let (mut runner, kickoff, _bolt, creature) =
        setup_2p_optional_beneficial(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();

    assert!(
        drive_until_marked(&mut runner, P0, 500),
        "B5-1: the optional self-lifegain cascade must reach the revocable-∞ mark"
    );

    // Path C is a silent mark: neither drawn nor crowned. The game continues at Priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "B5-1: an optional beneficial loop must fall through to a live priority window, \
         not GameOver; got {:?}",
        runner.state().waiting_for
    );
    let axes = runner
        .state()
        .unbounded_resources
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        axes.contains(&ResourceAxis::Life(P0)),
        "B5-1: P0's revocable-∞ capability must be marked on the Life axis; got {axes:?}"
    );
    let enablers = runner
        .state()
        .unbounded_loop_enablers
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        enablers.contains(&creature),
        "B5-1: the enabler set must include the life-engine creature; got {enablers:?}"
    );

    // Control (a): Off never marks — the sampler never records under Off (Interactive-only).
    let (mut orunner, okickoff, _ob, _oc) = setup_2p_optional_beneficial(LoopDetectionMode::Off);
    let _ = orunner.cast(okickoff).resolve();
    let _ = drive_collect(&mut orunner, 500);
    assert!(
        !orunner.state().unbounded_resources.contains_key(&P0),
        "Off must never populate unbounded_resources (Interactive-only)"
    );

    // Control (b): the mandatory sibling (same SELF_LIFE_ENGINE pattern, no opponent
    // action — `setup_3p_draw`) reaches Path B's draw, NOT a Path C mark — proves the
    // `!mandatory` gate discriminates, not merely "any beneficial loop marks."
    let (mut drunner, dkickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    let _ = drunner.cast(dkickoff).resolve();
    let (_de, dwf) = drive_collect(&mut drunner, 500);
    assert_eq!(
        dwf,
        WaitingFor::GameOver { winner: None },
        "control: the mandatory sibling must still draw via Path B"
    );
    assert!(
        !drunner.state().unbounded_resources.contains_key(&P0),
        "control: a mandatory draw (Path B) must not ALSO mark via Path C"
    );
}

/// B5-2: an enabler leaving the battlefield (a real zone change through the shared
/// `apply_zone_exit_cleanup` chokepoint) revokes the whole revocable-∞ capability.
#[test]
fn b5_2_enabler_departure_clears_the_mark() {
    let (mut runner, kickoff, bolt, creature) =
        setup_2p_optional_beneficial(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();

    assert!(
        drive_until_marked(&mut runner, P0, 500),
        "reach-guard: must be marked before testing the defuse"
    );
    assert!(
        runner
            .state()
            .unbounded_loop_enablers
            .get(&P0)
            .is_some_and(|e| e.contains(&creature)),
        "reach-guard: the creature must actually be a registered enabler"
    );

    // The driver may have stopped mid-cycle with P0 holding priority; advance to P1's
    // window so P1 (the Bolt's controller) can cast it.
    assert!(
        advance_to_player_priority(&mut runner, P1, 50),
        "must be able to reach P1's priority window to cast the Bolt"
    );

    let _ = runner.cast(bolt).target_object(creature).resolve();
    assert_ne!(
        runner.state().objects.get(&creature).map(|o| o.zone),
        Some(engine::types::zones::Zone::Battlefield),
        "the enabler creature must have left the battlefield (a real zone change)"
    );

    assert!(
        !runner.state().unbounded_resources.contains_key(&P0),
        "B5-2: the enabler's departure must clear unbounded_resources"
    );
    assert!(
        !runner.state().unbounded_loop_enablers.contains_key(&P0),
        "B5-2: the enabler's departure must clear unbounded_loop_enablers"
    );
}

/// Defuse-inert (Team-lead-B hard gate): under `Off`, the SAME real zone-change path
/// through `apply_zone_exit_cleanup` never populates or mutates either B5 map — the
/// empty-map guard makes the shared `zones.rs` hook a structural no-op.
#[test]
fn defuse_hook_inert_under_off() {
    let (mut runner, kickoff, bolt, creature) =
        setup_2p_optional_beneficial(LoopDetectionMode::Off);
    let _ = runner.cast(kickoff).resolve();
    let _ = drive_until(&mut runner, 50, |_| false);
    assert!(
        runner.state().unbounded_loop_enablers.is_empty(),
        "reach-guard: Off must never populate unbounded_loop_enablers (only the Interactive \
         B5 arm does) — this is what makes the defuse hook's guard a no-op below"
    );

    assert!(
        advance_to_player_priority(&mut runner, P1, 50),
        "must be able to reach P1's priority window to cast the Bolt"
    );
    let _ = runner.cast(bolt).target_object(creature).resolve();
    assert_ne!(
        runner.state().objects.get(&creature).map(|o| o.zone),
        Some(engine::types::zones::Zone::Battlefield),
        "positive reach-guard: the creature really did leave the battlefield under Off too"
    );

    assert!(
        runner.state().unbounded_resources.is_empty()
            && runner.state().unbounded_loop_enablers.is_empty(),
        "Off: both maps must stay empty across a real battlefield departure — the shared \
         zones.rs hook body never executes when the enabler map starts empty"
    );
}

/// LOW-2: the AI's `RespondToShortcut` decision self-preserves. Positive: the polled
/// opponent with a meaningful action (a castable Bolt) Shortens rather than Accepting its
/// own loss, and applying that response actually hands it a real priority window.
/// Control: the SAME fixture/flow's second APNAP responder — who holds no meaningful
/// action — gets Accept from the identical `smart_shortcut_response` call.
#[test]
fn low2_smart_shortcut_self_preservation() {
    // Positive: P1 (has the Bolt) self-preserves via Shorten.
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0);
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P0 declares");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "positive reach-guard: P1 must be prompted before the AI decision is tested"
    );

    let p1_response = engine::ai_support::smart_shortcut_response(runner.state(), P1);
    assert_eq!(
        p1_response,
        ShortcutResponse::Shorten { at_iteration: 0 },
        "P1 holds a meaningful action (Bolt) ⇒ smart_shortcut_response must self-preserve \
         via Shorten, not Accept its own loss"
    );
    runner
        .act(GameAction::RespondToShortcut {
            response: p1_response,
        })
        .expect("apply P1's AI decision");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "Shorten hands P1 a real priority window — it survives"
    );
    assert!(
        life(&runner, P1) > 0,
        "P1 is alive — the loop was not auto-taken"
    );

    // Control: the identical fixture/flow, but P1 Accepts (submitted manually, not via the
    // AI, so the APNAP queue advances instead of stopping) so the SECOND responder (P2,
    // who holds no meaningful action) is reached. `smart_shortcut_response` must Accept.
    let (mut crunner, ckickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = crunner.cast(ckickoff).resolve();
    let (_ce, cwf) = drive_collect(&mut crunner, 500);
    assert!(matches!(cwf, WaitingFor::LoopShortcut { .. }));
    crunner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("declare");
    assert!(
        matches!(
            crunner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "positive reach-guard: P1 is first in APNAP order"
    );
    crunner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts (manually, to advance the APNAP queue to P2)");
    assert!(
        matches!(
            crunner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "positive reach-guard: P2 is prompted second"
    );

    let p2_response = engine::ai_support::smart_shortcut_response(crunner.state(), P2);
    assert_eq!(
        p2_response,
        ShortcutResponse::Accept,
        "control: P2 holds no meaningful action ⇒ smart_shortcut_response must Accept \
         (revert-failing: an unconditional-Accept revert makes P1's response above Accept \
         too, which crowns P0's win with P1 still a faller — the Shorten assertion above \
         would fail first)"
    );
}

// ---------------------------------------------------------------------------
// PR-7 Phase 4d-ii — LIVE object-growth detection + offer (the 51st: Witherbloom,
// the Balancer + Sprout Swarm token-growth infinite). Cast-pipeline tests: real
// parsed AST (verbatim Oracle text), driven through `GameRunner::cast(..).resolve()`.
// ---------------------------------------------------------------------------

/// Sprout Swarm's verbatim Oracle text (Scryfall / card-data.json).
const SPROUT_SWARM_ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nBuyback {3} (You may pay an additional {3} as you cast this spell. If you do, put this card into your hand as it resolves.)\nCreate a 1/1 green Saproling creature token.";

/// Witherbloom's granted-affinity Oracle line (the loop-relevant clause).
const WITHERBLOOM_AFFINITY_ORACLE: &str =
    "Instant and sorcery spells you cast have affinity for creatures.";

/// Build the 51st fixture: Witherbloom (granted affinity) + `n_fodder` untapped green
/// 1/1 Saproling creatures + Sprout Swarm ({1}{G}, Buyback {3}, Convoke) in P0's hand.
/// Returns `(runner, sprout_id, fodder_ids)`. `Interactive` loop-detection ON.
fn sprout_swarm_scenario(n_fodder: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    sprout_swarm_scenario_with_drain(n_fodder, None)
}

/// As [`sprout_swarm_scenario`], but optionally adds a big "Test Drain Engine" permanent whose
/// `drain_oracle` (a `"Whenever you cast a spell, ..."` trigger) fires on EACH recast and drains
/// a resource axis in the LIVE recast body — the N4/N5/N6 no-offer negative controls. The engine
/// is a 9/9 so a self-damage drain does not kill it within the 2-iteration detection drive.
fn sprout_swarm_scenario_with_drain(
    n_fodder: usize,
    drain_oracle: Option<&str>,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    if let Some(oracle) = drain_oracle {
        scenario.add_creature_from_oracle(P0, "Test Drain Engine", 9, 9, oracle);
    }
    let mut fodder = Vec::new();
    for _ in 0..n_fodder {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        // The starting fodder must be GREEN so convoke can tap it for the {G} pip.
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    (runner, sprout, fodder)
}

/// Count real Saproling tokens/creatures on P0's battlefield in a state.
fn saproling_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.name == "Saproling"))
        .count()
}

/// P1 ⭐ — the 51st COVERS and OFFERS. A single real Witherbloom/Sprout-Swarm cast (paying
/// buyback and convoke) settles with an empty stack; the empty-stack hook drives two recast
/// iterations on a clone, confirms the fodder-growth cover and sign-check, and OFFERS the
/// interactive shortcut. Discriminators: the offer reaches `LoopShortcut`; and clone-isolation,
/// exactly ONE real Saproling was created by the single real cast (the drives ran on clones).
#[test]
fn object_growth_51st_sprout_swarm_covers_and_offers() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let before = saproling_count(runner.state());
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, predicted_winner, .. }
                if *proposer == P0 && predicted_winner.is_none()
        ),
        "expected LoopShortcut offer to P0, got {:?}",
        outcome.final_waiting_for()
    );
    let WaitingFor::LoopShortcut { certificate, .. } = outcome.final_waiting_for() else {
        unreachable!()
    };
    assert_eq!(
        certificate.win_kind,
        WinKind::Advantage,
        "an inert token-growth loop is a CR 104.4b optional Advantage loop"
    );
    assert!(
        certificate.unbounded.contains(&ResourceAxis::TokensCreated),
        "the unbounded axis must name TokensCreated, got {:?}",
        certificate.unbounded
    );
    // Clone-isolation (risk iii): the two detection drives ran on CLONES and must not
    // leak — exactly 4 starting + 1 from the single real cast = 5 real Saprolings.
    assert_eq!(
        saproling_count(outcome.state()),
        before + 1,
        "the clone drives must not leak real tokens (INV-1)"
    );
    // Sprout Swarm returned to hand (CR 702.27a buyback) — recastable for the loop.
    assert_eq!(outcome.zone_of(sprout), engine::types::zones::Zone::Hand);

    // N7 CAPTURE-side (live, seam-not-line): the foundation's `fodder_cover_last_loop_action_sequence_
    // two_sided` proves the COMPARE (`eq_except_growable`) rejects a heterogeneous context, but
    // it CONSTRUCTS the field by hand — it cannot prove the live capture at
    // `finalize_cast_with_phyrexian_choices` writes DISCRIMINATING values (a wrong-but-constant
    // capture would pass P1's offer and the foundation test both). Assert the captured context
    // holds the real cast's discriminating fields, so a constant/wrong capture fails here.
    let ctx = outcome
        .state()
        .last_loop_action_sequence
        .first()
        .expect("buyback + token-creating cast must capture a loop-action context");
    assert_eq!(ctx.controller, P0);
    let engine::types::game_state::LoopAction::Recast {
        from_zone,
        uses_buyback,
        ..
    } = &ctx.action
    else {
        panic!("a buyback token cast must capture a Recast loop action");
    };
    assert_eq!(
        *from_zone,
        engine::types::zones::Zone::Hand,
        "CR 601.2a: buyback returns the spell to hand ⇒ from_zone is Hand"
    );
    assert_eq!(
        *uses_buyback,
        engine::types::game_state::BuybackUsage::Used,
        "the captured context records that buyback was paid"
    );
    assert_eq!(
        ctx.convoke,
        Some(engine::types::game_state::ConvokeMode::Convoke),
        "Sprout Swarm has Convoke ⇒ the convoke mode is derived from the keyword, not a constant"
    );
    // card_id is the real recastable Sprout Swarm's identity (CR 400.7), not the churned ObjectId.
    let hand_sprout = outcome
        .state()
        .objects
        .values()
        .find(|o| {
            o.name == "Sprout Swarm"
                && o.controller == P0
                && o.zone == engine::types::zones::Zone::Hand
        })
        .expect("Sprout Swarm recastable in hand");
    assert_eq!(
        ctx.card_id, hand_sprout.card_id,
        "captured card_id is the real recast card's CR 400.7 identity"
    );
}

/// Kodama of the East Tree's growing-class-reading trigger (Scryfall / card-data).
/// Its body puts a permanent "with equal or lesser mana value" from hand onto the
/// battlefield — a `ChangeZone` whose target filter reads a mutable board aggregate,
/// so `fire_time_conditions_read_growing_class` flags it IF it is scanned.
const KODAMA_TRIGGER_ORACLE: &str = "Whenever another permanent you control enters, if it wasn't put onto the battlefield with this ability, you may put a permanent card with equal or lesser mana value from your hand onto the battlefield.";

/// REGRESSION (user 2026-07-18): a growing-class-reading trigger sitting in a zone
/// where it CANNOT function (here P0's LIBRARY) must NOT suppress the loop-shortcut
/// offer. This reproduces the real 4-player game where Witherbloom + Sprout Swarm
/// failed to prompt because Kodama of the East Tree — a deck card in the library —
/// was scanned by the object-growth cover's `fire_time_conditions_read_growing_class`
/// firewall as if it were a live observer (CR 603.4 / CR 113.6: a permanent trigger
/// functions only on the battlefield). The board is otherwise the passing 51st
/// fixture, so the ONLY variable is the inert library observer.
///
/// DISCRIMINATING (revert-probe verified): reverting the block-(1) zone gate in
/// `fire_time_conditions_read_growing_class` flips this to NO offer — Kodama's
/// library trigger is re-scanned, `cover_ok` goes false, and `final_waiting_for`
/// stays `Priority`. So this fails without the fix.
#[test]
fn object_growth_library_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    // Kodama parses ON the battlefield (so its trigger is a real parsed def), then we
    // relocate it into the library below — where it cannot function.
    let kodama = scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 6, KODAMA_TRIGGER_ORACLE)
        .id();
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
        // Move Kodama from the battlefield into P0's LIBRARY (CR 603.4: its
        // "another permanent enters" trigger no longer functions there).
        st.battlefield.retain(|&id| id != kodama);
        let obj = st.objects.get_mut(&kodama).unwrap();
        obj.zone = Zone::Library;
        let p0 = st.players.iter_mut().find(|p| p.id == P0).unwrap();
        p0.library.insert(0, kodama);
    }

    // Sanity: Kodama really is in the library (not the battlefield), so any offer
    // must come from correctly IGNORING it, not from it having been removed.
    let kodama_obj = &runner.state().objects[&kodama];
    assert_eq!(
        kodama_obj.zone,
        Zone::Library,
        "the growing-class observer must sit in the library for this to discriminate",
    );
    assert_eq!(
        kodama_obj.trigger_definitions.len(),
        1,
        "reach-guard: the observer must have PARSED — a misparse leaves zero trigger \
         defs and the offer below forms for the wrong reason (nothing to ignore); got {}",
        kodama_obj.trigger_definitions.len()
    );

    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "a growing-class trigger in the LIBRARY must not suppress the offer, got {:?}",
        outcome.final_waiting_for()
    );
    // The offer still names the token-growth axis (the loop is genuinely detected,
    // not an unrelated fall-through).
    let WaitingFor::LoopShortcut { certificate, .. } = outcome.final_waiting_for() else {
        unreachable!()
    };
    assert!(
        certificate.unbounded.contains(&ResourceAxis::TokensCreated),
        "the detected loop's unbounded axis must be TokensCreated, got {:?}",
        certificate.unbounded
    );
}

/// Find the (single) object named `name` controlled by `player` in `zone`.
fn object_named_in_zone(
    state: &GameState,
    name: &str,
    player: PlayerId,
    zone: engine::types::zones::Zone,
) -> Option<ObjectId> {
    state
        .objects
        .values()
        .find(|o| o.name == name && o.controller == player && o.zone == zone)
        .map(|o| o.id)
}

/// P2 ⭐ (updated 2026-07-18, user directive): ACCEPTING an unbounded object-growth (fodder /
/// token) shortcut MARKS the certificate's ∞ axes via the shared `mark_unbounded_loop` writer
/// and materializes ZERO discrete tokens — the ∞ status IS the applied result (contrast the
/// old O(N) drive, which capped the infinite at N and cost ≈0.4 s/token / 212 s for 500). The
/// finite tokens are minted later, at the CR 500.4 phase/turn boundary, when the player names a
/// finite count for each ∞ status; accept itself only flags the status. Declaring `Fixed(5)`
/// yet getting 0 tokens is itself discriminating — it proves the count is ignored (no drive).
///
/// DISCRIMINATING (revert-probe verified): deleting the `mark_unbounded_loop` call in
/// `materialize_object_growth_shortcut` leaves `unbounded_resources` empty ⇒ the ∞-status
/// assertion FLIPS to fail ("must mark unbounded_resources"); a re-introduced N-iteration drive
/// would break the board-invariance assertion (it would add ≥1 Saproling).
#[test]
fn object_growth_51st_accept_marks_unbounded_and_mints_no_tokens() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    let WaitingFor::LoopShortcut { certificate, .. } = outcome.final_waiting_for() else {
        panic!(
            "P2 precondition: the offer must fire, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(certificate.unbounded.contains(&ResourceAxis::TokensCreated));
    assert!(
        runner.state().unbounded_resources.is_empty(),
        "the OFFER must not pre-mark the ∞ status (only accepting does)"
    );
    let at_offer = saproling_count(runner.state());

    // P0 (LoopShortcut.proposer — inferred submitter) declares a Fixed(5) shortcut. The count
    // is ignored for an unbounded loop (the ∞ mark is count-independent); Fixed(5) here is a
    // discriminator that a re-introduced drive would turn into +5 tokens.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(5),
            template: None,
        })
        .expect("declare shortcut");
    // The lone opponent (P1 — inferred RespondToShortcut submitter) accepts ⇒ mark ∞.
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("respond accept");

    // (1) ∞ status APPLIED — the revert-probe target.
    let axes = runner
        .state()
        .unbounded_resources
        .get(&P0)
        .expect("accepting an unbounded loop must mark unbounded_resources for the controller");
    assert!(
        axes.contains(&ResourceAxis::TokensCreated),
        "the marked axis must be TokensCreated, got {axes:?}"
    );
    // (2) ZERO tokens minted at accept — the finite count is named later, at the phase boundary.
    assert_eq!(
        saproling_count(runner.state()),
        at_offer,
        "accepting an unbounded loop must not drive discrete iterations"
    );
    assert!(
        object_named_in_zone(
            runner.state(),
            "Sprout Swarm",
            P0,
            engine::types::zones::Zone::Hand
        )
        .is_some(),
        "CR 702.27a: Sprout Swarm must still be in P0's hand after accept"
    );
    // (3) priority handed back to a living seat (CR 800.4a) — the protocol closed cleanly.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority handed back after accept, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().loop_detect_ring.is_empty());
}

/// T-object-growth-decline ⭐ (Seam 2): CR 732.2a — the controller DECLINES the auto-offered
/// object-growth (Sprout Swarm) shortcut. The engine restores ordinary priority, clears the
/// object-growth routing context, an ordinary action resolves, and the loop is NOT re-offered.
///
/// Non-vacuous, two-seam-independent revert-probe: this offer is gated by
/// `!last_loop_action_sequence.is_empty()` (engine.rs Seam 2), so `last_loop_action_sequence.clear()`
/// in `handle_decline_shortcut` is the SOLE load-bearing suppression here (the ring is empty on
/// this path, so deleting `loop_detect_ring.clear()` has no effect). Deleting
/// `last_loop_action_sequence.clear()` leaves the routing sequence set ⇒ the post-return reconcile
/// re-fires `try_offer_object_growth_shortcut` within this same `apply()` ⇒ the `Priority`
/// assertion flips back to `LoopShortcut`. (Distinct from the interactive test's probe line ⇒
/// the two seams are covered independently.)
#[test]
fn object_growth_sprout_swarm_decline_restores_priority_no_reoffer() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let _ = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    // F2 positive reach-guard: the object-growth offer was genuinely reached, and its routing
    // context is set (the Seam-2 gate the decline must clear).
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { proposer, predicted_winner, .. } if proposer == P0 && predicted_winner.is_none()),
        "Sprout Swarm must OFFER a LoopShortcut to P0, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().last_loop_action_sequence.is_empty(),
        "the object-growth offer must have captured a recast context (the Seam-2 gate)"
    );

    // RIDER-3 (runtime, semantic identity): the engine-owned `convoke_tappable_count` published on
    // the offer schema must equal the sum the DELETED React reduce computed over the same points
    // (Sprout Swarm convokes ⇒ a real nonzero ConvokeTaps offer). Cross-checking the published
    // count against the live ConvokeTaps `tappable` lengths proves the authority-move to the
    // engine changed no displayed value — a wrong/defaulted engine count would fail here.
    if let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for {
        let react_equivalent: usize = schema
            .points
            .iter()
            .filter_map(|p| match &p.kind {
                DecisionPointKind::ConvokeTaps { tappable } => Some(tappable.len()),
                _ => None,
            })
            .sum();
        assert!(
            react_equivalent > 0,
            "Sprout Swarm's object-growth offer must present a real nonzero ConvokeTaps schema"
        );
        assert_eq!(
            schema.convoke_tappable_count, react_equivalent,
            "engine-owned convoke_tappable_count must equal the old React reduce's sum over the same points (RIDER-3)"
        );
    }

    // CR 732.2a: the controller (P0) declines the offer.
    let decline = runner
        .act(GameAction::DeclineShortcut)
        .expect("P0 declines the object-growth shortcut");

    // (a) + (c): ordinary priority restored AND the Seam-2 routing context cleared, so the
    // post-return reconcile does not re-fire `try_offer_object_growth_shortcut`. With
    // `last_loop_action_sequence = None` reverted, the intact context re-offers ⇒ this flips to
    // `LoopShortcut`.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "decline restores ordinary priority; the context-clear suppresses the immediate re-offer, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(decline.waiting_for, WaitingFor::Priority { .. }),
        "the decline result hands priority back"
    );
    assert!(
        runner.state().last_loop_action_sequence.is_empty(),
        "the object-growth routing context was cleared on decline (Seam-2 revert-probe line)"
    );
    assert!(runner.state().loop_detect_ring.is_empty());

    // (b) an ordinary action resolves from the restored priority window.
    runner
        .act(GameAction::PassPriority)
        .expect("an ordinary PassPriority resolves after the decline handback");

    // (c) the declined loop is not instantly re-offered on the immediate next beat.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "the declined object-growth loop must not be re-offered, got {:?}",
        runner.state().waiting_for
    );
}

/// N1 — finite-mana REJECTS (B4). Same fixture WITHOUT Witherbloom's affinity granter: each
/// recast must pay the real {1}{G}+buyback{3} = {4}{G}, which 4 untapped green creatures
/// cannot cover by convoke alone (needs 5 taps) ⇒ the injector aborts (UnpayableConvoke) ⇒
/// no offer. Revert-failing paired reach-guard: P1 (with affinity) DOES offer, so the only
/// difference is the affinity reduction feeding the sustainable {G}-only convoke cost.
#[test]
fn object_growth_no_affinity_does_not_offer() {
    // Fixture with NO Witherbloom (no affinity): 4 green Saprolings + Sprout Swarm, plus a
    // pool that funds ONE manual cast of {4}{G} so the first cast still resolves and captures
    // the recast context — isolating the DRIVEN recast's unpayability as the discriminator.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    // Fund the FIRST cast entirely from the pool ({4} generic + {G}); no convoke needed, so
    // the first cast resolves + captures the recast context, isolating the DRIVEN recast's
    // convoke-only unpayability as the sole discriminator.
    let mut mana = vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 4];
    mana.push(ManaUnit::new(
        ManaType::Green,
        ObjectId(9_999),
        false,
        vec![],
    ));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    let outcome = runner.cast(sprout).accept_optional().commit().resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no affinity ⇒ the driven recast can't afford {{4}}{{G}} via convoke ⇒ NO offer, got {:?}",
        outcome.final_waiting_for()
    );
}

// ---------------------------------------------------------------------------
// INTERRUPTIBILITY matched pair (combo 2+3): the opponent HOLDS a real Murder.
// Undefused (opponent passes) ⇒ the CR 732.2a object-growth shortcut is GRANTED.
// Defused (opponent Murders Witherbloom in response to Sprout Swarm on the
// stack, CR 601.2i) ⇒ the affinity granter is gone, so the empty-stack hook's
// clone-drive re-derives the recast WITHOUT affinity, convoke alone can't pay
// {4}{G} ⇒ NO grant beyond the current stack. The opponent's pass-vs-respond is
// the SOLE delta and FLIPS the outcome.
// ---------------------------------------------------------------------------

/// Arm `player` with a real castable Murder ({1}{B}{B}, "Destroy target creature.") backed by 3
/// Swamps — the held defuse. Returns the Murder's `ObjectId`.
fn arm_murder(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    for _ in 0..3 {
        scenario.add_basic_land(player, ManaColor::Black);
    }
    let mut murder =
        scenario.add_spell_to_hand_from_oracle(player, "Murder", true, "Destroy target creature.");
    murder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 1,
    });
    murder.id()
}

/// As [`sprout_swarm_scenario`], but ALSO arms P1 with a held Murder (the defuse for the CR 732.2a
/// interruptibility pair). Returns `(runner, sprout, witherbloom, murder, fodder)`.
///
/// R3: pinned at `n_fodder = 4`. The defused negative relies on the driven recast being unpayable
/// once affinity is removed — convoke-only must then pay the full {4}{G} (buyback {3} + base
/// {1}{G}), i.e. 5 taps, while at most 4 untapped green creatures remain at the recast (one fodder
/// is tapped for the real cast's convoke, plus the one fresh Saproling). If a future bump made
/// convoke alone able to pay {4}{G}, the Murder defuse would stop breaking the loop and the defused
/// test would go vacuous — keep this tied to the `object_growth_no_affinity_does_not_offer` math.
fn sprout_swarm_scenario_with_murder(
    n_fodder: usize,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witherbloom = scenario
        .add_creature_from_oracle(
            P0,
            "Witherbloom, the Balancer",
            5,
            5,
            WITHERBLOOM_AFFINITY_ORACLE,
        )
        .id();
    let mut fodder = Vec::new();
    for _ in 0..n_fodder {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let murder = arm_murder(&mut scenario, P1);
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    (runner, sprout, witherbloom, murder, fodder)
}

/// T-object-growth-INT-a ⭐ — INTERRUPTIBILITY, UNDEFUSED: P1 HOLDS a real Murder but PASSES ⇒ the
/// CR 732.2a object-growth shortcut is GRANTED. Sprout Swarm resolves through a genuine response
/// window (P1 auto-passes, CR 601.2i/117.3c), the token-growth loop settles, and the shortcut is
/// OFFERED. Matched with the defused twin: P1's pass-vs-respond is the SOLE delta and FLIPS the
/// outcome. Reach-guards prove the defuse was genuinely held (Murder still in hand, Witherbloom
/// still on the battlefield).
#[test]
fn object_growth_interruptibility_undefused_opponent_passes_grants() {
    let (mut runner, sprout, witherbloom, murder, fodder) = sprout_swarm_scenario_with_murder(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "UNDEFUSED (P1 passes): the object-growth shortcut is OFFERED to P0, got {:?}",
        outcome.final_waiting_for()
    );
    // Reach-guards: the defuse was genuinely HELD (not spent) and the affinity granter survived.
    assert_eq!(
        outcome.state().objects[&murder].zone,
        engine::types::zones::Zone::Hand,
        "P1's Murder is still in hand (held, not cast) — the offer is not vacuous on a spent defuse"
    );
    assert_eq!(
        outcome.state().objects[&witherbloom].zone,
        engine::types::zones::Zone::Battlefield,
        "Witherbloom (the affinity granter) survives when P1 passes"
    );
}

/// T-object-growth-INT-b ⭐ — INTERRUPTIBILITY, DEFUSED: P1 RESPONDS to Sprout Swarm (on the stack,
/// CR 601.2i) by casting Murder on Witherbloom. The affinity granter is destroyed, Sprout resolves
/// (one Saproling made, buyback → hand), and the empty-stack hook's clone-drive re-derives the
/// recast WITHOUT affinity ⇒ convoke-only {4}{G} needs 5 taps but ≤4 untapped greens remain ⇒
/// unpayable ⇒ NO grant beyond the current stack (CR 732.2a). The ONLY delta vs the undefused twin
/// is P1's respond-vs-pass, and the outcome FLIPS (offer → no offer). This is the exact
/// `object_growth_no_affinity_does_not_offer` mechanism, reached at RUNTIME by removing affinity
/// mid-stack instead of omitting the granter from the fixture.
#[test]
fn object_growth_interruptibility_defused_opponent_responds_no_grant() {
    let (mut runner, sprout, witherbloom, murder, fodder) = sprout_swarm_scenario_with_murder(4);
    let before = saproling_count(runner.state());
    let murder_card = runner.state().objects[&murder].card_id;

    // Commit Sprout (buyback + convoke) to the stack WITHOUT resolving — leaving P0 priority with
    // Sprout on the stack. The bare `commit()` temporary is dropped at the `;`, releasing the
    // borrow so the manual drive can continue.
    runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit();
    // P0 passes ⇒ P1 gets priority with Sprout on the stack (the real response window).
    runner.act(GameAction::PassPriority).expect("P0 passes");
    // P1 RESPONDS: Murder destroys Witherbloom in response to Sprout. The reducer surfaces a
    // `TargetSelection` prompt (the action's `targets` field is not consumed), answered below.
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id: murder_card,
            targets: vec![witherbloom],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 may cast Murder in response (instant speed)");
    // Settle: Murder targets Witherbloom, resolves, destroys it; then Sprout resolves (token +
    // buyback → hand); then the empty-stack hook drives the clone (no affinity ⇒ unpayable ⇒ no
    // offer).
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(witherbloom)],
                    })
                    .expect("Murder targets Witherbloom (a legal creature)");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    // Reach-guards: the response LANDED (Witherbloom gone) and the Sprout cast still RESOLVED (a
    // real Saproling was made — the no-offer is the recast-unpayable break, not a fizzled cast).
    assert!(
        runner.state().objects.get(&witherbloom).map(|o| o.zone)
            != Some(engine::types::zones::Zone::Battlefield),
        "reach-guard: P1's Murder destroyed Witherbloom (the response landed)"
    );
    assert_eq!(
        saproling_count(runner.state()),
        before + 1,
        "reach-guard: Sprout still resolved and made one Saproling (the cast did not fizzle)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "DEFUSED (P1 responds): affinity is gone ⇒ the driven recast can't afford {{4}}{{G}} via \
         convoke ⇒ NO grant beyond the current stack, got {:?}",
        runner.state().waiting_for
    );
}

/// As [`setup_2p_vito_optional`], but ALSO arms P1 with a held Murder (the defuse for the CR 732.2a
/// Vito-drain interruptibility pair) and captures the Bloodthirsty Conqueror + Murder ids.
///
/// Sanguine Bond is a REDUNDANT drainer: the drain loop is Vito+Conqueror OR Sanguine+Conqueror
/// (either targeted drainer feeds the single closer). Bloodthirsty Conqueror is the SINGLE closer
/// ("Whenever an opponent loses life, you gain that much life") — Murder→Conqueror breaks the loop
/// regardless of the redundant Sanguine (drop-probe-confirmed by the spike). Both drainers still
/// fire per P0 lifegain, so the per-window life decrement is 2 (Vito's 1 + Sanguine's 1).
fn setup_2p_vito_optional_with_murder(
    mode: LoopDetectionMode,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 4, VITO);
    scenario.add_creature_from_oracle(P0, "Sanguine Bond", 2, 2, SANGUINE_BOND);
    let conqueror = scenario
        .add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR)
        .id();
    // The red land + Bolt make the loop OPTIONAL (so it OFFERS instead of auto-crowning); keep it.
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let murder = arm_murder(&mut scenario, P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff, conqueror, murder)
}

/// T-Vito-INT-a ⭐ — INTERRUPTIBILITY, UNDEFUSED: P1 HOLDS a real Murder but PASSES ⇒ the CR 732.2a
/// Vito-drain shortcut is GRANTED. The kickoff resolves, the Vito/Sanguine drains fan out through
/// genuine APNAP priority windows (P1 auto-passes, CR 601.2i/117.3c), the loop settles, and the
/// shortcut is OFFERED to P0. Matched with the defused twin: P1's pass-vs-respond is the SOLE delta
/// and FLIPS the outcome. Reach-guards prove the defuse was genuinely held (Murder still in hand,
/// closer still on the battlefield, P1 alive-positive).
#[test]
fn vito_interruptibility_undefused_opponent_passes_grants() {
    let (mut runner, kickoff, conqueror, murder) =
        setup_2p_vito_optional_with_murder(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 2000);

    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
        ..
    } = wf
    else {
        panic!("UNDEFUSED (P1 passes): the optional 2p Vito drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    // The Vito drain's deciding win is lethal (the drain kills P1), not an inert Advantage loop.
    assert_eq!(
        certificate.win_kind,
        WinKind::LethalDamage,
        "the Vito drain offer's deciding win_kind is LethalDamage"
    );
    assert!(
        !certificate.mandatory,
        "the loop is OPTIONAL (P1 holds real answers)"
    );
    // Reach-guards: the defuse was genuinely HELD (Murder still in hand, not spent) and the single
    // closer survived — so the offer is not vacuous on a spent defuse / broken loop.
    assert_eq!(
        runner.state().objects[&murder].zone,
        engine::types::zones::Zone::Hand,
        "P1's Murder is still in hand (held, not cast)"
    );
    assert_eq!(
        runner.state().objects[&conqueror].zone,
        engine::types::zones::Zone::Battlefield,
        "Bloodthirsty Conqueror (the single closer) survives when P1 passes"
    );
    assert!(
        life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
        "the offer fires EARLY with P1 alive-positive, life = {}",
        life(&runner, P1)
    );
}

/// T-Vito-INT-b ⭐ — INTERRUPTIBILITY, DEFUSED: P1 RESPONDS at the first pre-offer priority window
/// that has the Vito/Sanguine drains on the stack (CR 603.3b) by casting Murder on Bloodthirsty
/// Conqueror. The single closer is destroyed; the 2 in-flight drains then resolve (P1 loses EXACTLY
/// 2, no Conqueror re-gain) and the stack empties ⇒ NO grant beyond the current stack (CR 732.2a).
/// The ONLY delta vs the undefused twin is P1's respond-vs-pass, and the outcome FLIPS (offer → no
/// offer). Non-vacuity reach-guards: the response LANDED (Conqueror → graveyard), the defuse was
/// spent (Murder left P1's hand), and P1 lost EXACTLY 2 — the precise decrement proves the 2 drains
/// fired before the closer-removal break (so no-offer is the break, not an upstream fizzle).
#[test]
fn vito_interruptibility_defused_opponent_responds_no_grant() {
    let (mut runner, kickoff, conqueror, murder) =
        setup_2p_vito_optional_with_murder(LoopDetectionMode::Interactive);
    let initial_p1_life = life(&runner, P1);
    let murder_card = runner.state().objects[&murder].card_id;

    // Commit the kickoff to the stack WITHOUT resolving — P0 retains priority with it on the stack.
    runner.cast(kickoff).commit();

    // STEP to the FIRST Priority{P1} window whose stack carries a Vito/Sanguine drain trigger
    // (CR 603.3b: the drains sit on the stack after the kickoff resolves, giving P1 a genuine
    // pre-offer response window). Do NOT auto-pass P1 there. Before that window the only stack
    // entry is the kickoff Spell (no TriggeredAbility), so this precisely selects the drain window.
    let mut reached = false;
    for _ in 0..80 {
        let (wf, drain_on_stack) = {
            let st = runner.state();
            (
                st.waiting_for.clone(),
                st.stack
                    .iter()
                    .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            )
        };
        match wf {
            WaitingFor::Priority { player } if player == P1 && drain_on_stack => {
                reached = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to advance toward the drain window");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("P0 orders its two simultaneous drain triggers");
            }
            other => panic!("unexpected state before the drain window: {other:?}"),
        }
    }
    assert!(
        reached,
        "must reach a Priority{{P1}} window with a drain trigger on the stack; got {:?}",
        runner.state().waiting_for
    );
    // Reach-guard: at the response window P1 has NOT yet lost life (drains unresolved) and the
    // closer is still live — the loss below is caused by the in-flight drains, not a prior cycle.
    assert_eq!(
        life(&runner, P1),
        initial_p1_life,
        "P1 has not lost life yet at the response window (drains still on the stack)"
    );

    // P1 RESPONDS: Murder destroys the single closer (Bloodthirsty Conqueror) in response to the
    // drains. The reducer surfaces a `TargetSelection` (the action's `targets` is not consumed),
    // answered in the settle loop below.
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id: murder_card,
            targets: vec![conqueror],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 may cast Murder in response (instant speed)");

    // Settle: Murder resolves (destroys Conqueror), then the 2 drains resolve (P1 -2, no re-gain),
    // then the stack empties. No new triggers fire (the closer is gone) ⇒ no offer.
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(conqueror)],
                    })
                    .expect("Murder targets Bloodthirsty Conqueror (a legal creature)");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    // Reach-guards (non-vacuity): the response LANDED (closer destroyed), the defuse was spent, and
    // the 2 in-flight drains resolved — the EXACT decrement proves the break happened after the
    // drains fired (not an upstream fizzle) and that the closer removal stopped the re-gain.
    assert_eq!(
        runner.state().objects[&conqueror].zone,
        engine::types::zones::Zone::Graveyard,
        "reach-guard: P1's Murder destroyed the closer (Conqueror → graveyard)"
    );
    assert_ne!(
        runner.state().objects[&murder].zone,
        engine::types::zones::Zone::Hand,
        "reach-guard: the defuse was spent (Murder left P1's hand)"
    );
    assert_eq!(
        life(&runner, P1),
        initial_p1_life - 2,
        "reach-guard: the 2 in-flight drains resolved (P1 lost EXACTLY 2, no Conqueror re-gain), \
         life = {}",
        life(&runner, P1)
    );
    // Terminal is NOT a LoopShortcut and IS a plain empty-stack Priority (the break, not a fizzle).
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "DEFUSED (P1 responds Murder→Conqueror): the single closer is gone ⇒ the drains resolve \
         once and the loop is broken ⇒ NO grant beyond the current stack, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "terminal is a plain empty-stack Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// N3 — no-buyback REJECTS (B3). Sprout Swarm cast WITHOUT paying buyback ⇒ the spell goes to
/// the graveyard, not hand ⇒ (a) `last_loop_action_sequence` is never captured (gate requires
/// `additional_cost_paid`), and (b) even were it captured, the injector's per-cycle re-find
/// in `ctx.from_zone` (Hand) would abort. Either way: no offer. Revert-failing paired
/// reach-guard: P1 (buyback paid, card returns to hand) DOES offer.
#[test]
fn object_growth_no_buyback_does_not_offer() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    // Decline buyback; convoke still pays the base {1}{G} (affinity reduces {1}→{0}).
    let outcome = runner
        .cast(sprout)
        .decline_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no buyback ⇒ card to graveyard ⇒ no recast context ⇒ NO offer, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().last_loop_action_sequence.is_empty(),
        "B3: last_loop_action_sequence must NOT be captured when buyback is unpaid"
    );
    // Reach-guard: confirm the cast actually resolved (a real Saproling was made), so the
    // negative above is not vacuous on an aborted cast.
    assert_eq!(
        saproling_count(outcome.state()),
        5,
        "the base cast still created one token"
    );
}

/// FIX 1 (#4603 opt-in gate): the RecastContext capture is gated on `loop_detection.samples()`,
/// so DEFAULT/OFF mode never writes `last_loop_action_sequence` — keeping the serialized surface
/// byte-identical to pre-PR-7 (the field is `skip_serializing_if=is_none`). Paired reach-guard:
/// the SAME buyback + token cast in Interactive (sampling) mode DOES capture `Some(..)`, proving
/// the OFF assertion is not vacuous on a cast that simply never captures.
#[test]
fn off_mode_capture_leaves_recast_context_none() {
    // OFF (default): flip the fixture's mode back to Off before the identical cast.
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    runner.state_mut().loop_detection = LoopDetectionMode::Off;
    let off = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        off.state().last_loop_action_sequence.is_empty(),
        "OFF (#4603): a buyback+token cast must NOT write last_loop_action_sequence on the serialized surface"
    );

    // ON/sampling reach-guard: the same cast captures Some(..) (else the OFF assertion is vacuous).
    let (mut on_runner, on_sprout, on_fodder) = sprout_swarm_scenario(4);
    let on = on_runner
        .cast(on_sprout)
        .accept_optional()
        .convoke_with(&[on_fodder[0]])
        .commit()
        .resolve();
    assert!(
        !on.state().last_loop_action_sequence.is_empty(),
        "Interactive/sampling: the same buyback+token cast DOES capture the recast context"
    );
}

/// N6 (CR 704.5g, branch d) — LIVE no-offer control. Each recast fires a
/// `"Whenever you cast a spell, ~ deals 1 damage to itself"` trigger on the controller's 9/9
/// engine, so the controller-side `damage_marked` total STRICTLY increases s_n1→s_n2. A
/// board-growing loop that also accrues damage on its own engine is self-terminating, not a
/// CR 732.2a shortcut, so `driving_resources_non_decreasing` branch (d) vetoes ⇒ NO offer.
/// Discriminating: revert-probe (delete branch (d)) ⇒ this WRONGLY offers. Paired reach-guard:
/// the same base loop WITHOUT the drain (P1's scenario) DOES offer.
#[test]
fn object_growth_self_damage_recast_does_not_offer() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario_with_drain(
        4,
        Some("Whenever you cast a spell, Test Drain Engine deals 1 damage to Test Drain Engine."),
    );
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::LoopShortcut { .. }),
        "N6: a damage-accruing recast is self-terminating (CR 704.5g) ⇒ must NOT offer, got {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guard: the same base loop without the drain reaches the offer.
    let (mut ok_runner, ok_sprout, ok_fodder) = sprout_swarm_scenario(4);
    let ok = ok_runner
        .cast(ok_sprout)
        .accept_optional()
        .convoke_with(&[ok_fodder[0]])
        .commit()
        .resolve();
    assert!(
        matches!(ok.final_waiting_for(), WaitingFor::LoopShortcut { .. }),
        "reach-guard: without the self-damage drain the same loop offers"
    );
}

fn sprout_shell_scenario(body: &str) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let oracle = format!(
        "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {{1}} or one mana of that creature's color.)\nBuyback {{3}} (You may pay an additional {{3}} as you cast this spell. If you do, put this card into your hand as it resolves.)\n{body}"
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, &oracle);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    (runner, sprout, fodder)
}

/// A2 (CR 732.2a determinism gate) — RECAST-BODY randomness NO-OFFER control. The recast spell's
/// own resolution body creates the fodder token AND flips a coin (CR 705.1). The board still grows
/// deterministically by one Saproling per cycle (so the fodder cover + sign-check pass), but the
/// coin makes the loop outcome-dependent ⇒ NOT a legal CR 732.2a shortcut ⇒ NO offer.
///
/// Why this fixture (not an external coin trigger): the fodder cover's
/// `fire_time_conditions_read_growing_class` already rejects a randomness-bearing *permanent*
/// ability (coin/die classify `Axes::CONSERVATIVE`), so an external coin trigger is caught by the
/// cover regardless of A2 — it cannot discriminate A2. The cover does NOT scan the resolving
/// recast *spell's* body, so a coin flip there is exactly the gap A2 closes; MEASURED: with BOTH
/// A2 halves reverted this fixture wrongly OFFERS (the coin advances the RNG 2→6 yet the cover
/// passes). Each A2 half independently rejects it: the static scan (a) bails pre-drive
/// (`spell_ability_bears_randomness`), and the runtime rng-position check (b) bails post-drive.
///
/// Non-vacuity: (1) item-5 — the body parses to `Token` (deterministic growth) + a `FlipCoin`
/// sub-effect (asserted below), so the coin genuinely fires; (2) revert-probe — reverting BOTH A2
/// halves flips this to an OFFER; (3) reach-guard — the SAME shell with a coin-free body offers,
/// isolating the coin (not the shell) as the disqualifier.
#[test]
fn object_growth_random_recast_body_does_not_offer() {
    // item-5: verify the recast body carries a deterministic Token AND a FlipCoin (so the board
    // grows each cycle while the coin advances the RNG — else the fixture would be vacuous).
    let body_def = engine::parser::oracle_effect::parse_effect_chain(
        "Create a 1/1 green Saproling creature token. Flip a coin.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert!(
        matches!(*body_def.effect, Effect::Token { .. }),
        "recast body head must be Token (deterministic growth), got {:?}",
        body_def.effect
    );
    assert!(
        body_def
            .sub_ability
            .as_ref()
            .is_some_and(|s| matches!(*s.effect, Effect::FlipCoin { .. })),
        "recast body must carry a FlipCoin sub-effect (the randomness A2 rejects), got {:?}",
        body_def.sub_ability
    );

    let (mut runner, sprout, fodder) =
        sprout_shell_scenario("Create a 1/1 green Saproling creature token. Flip a coin.");
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::LoopShortcut { .. }),
        "A2: a recast body bearing a coin flip is outcome-dependent (CR 732.2a) ⇒ must NOT offer, \
         got {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guard: the SAME shell with a coin-free body offers, isolating the coin (not the
    // buyback/convoke shell) as the sole disqualifier — and proving the input reaches the offer path.
    let (mut ok_runner, ok_sprout, ok_fodder) =
        sprout_shell_scenario("Create a 1/1 green Saproling creature token.");
    let ok = ok_runner
        .cast(ok_sprout)
        .accept_optional()
        .convoke_with(&[ok_fodder[0]])
        .commit()
        .resolve();
    assert!(
        matches!(ok.final_waiting_for(), WaitingFor::LoopShortcut { .. }),
        "reach-guard: the same shell with a deterministic (coin-free) body offers"
    );
}

// ── N4 (energy, branch a) + N5 (player-counter, branch b): UNIT + structural-wiring coverage,
// NOT live fixtures — a LIVE per-recast drain on these two axes is architecturally infeasible in
// this harness (team-lead-authorized fallback on GENUINE infeasibility, not convenience):
//   • Energy is only spent via a cost. Adding a per-cast energy cost to the recast breaks
//     Buyback's return-to-hand (measured: the spell does not return ⇒ the loop cannot recur), so
//     any resulting "no offer" comes from NON-RECURRENCE, not the branch-(a) energy sign-check —
//     a vacuous live test. (Revert-probing branch (a) did NOT flip such a fixture, confirming the
//     vacuity; it was removed rather than shipped as false confidence.)
//   • No engine effect decreases Experience/Ticket player-counters (only Rad, an automatic
//     precombat turn action, not a per-cast cost), so branch (b) has no live per-recast drain.
// Both branches are covered by the 4d-i foundation unit tests
// `analysis::resource::sign_check_energy_decrease_rejects` / `_player_counter_decrease_rejects`,
// and the live call-site (`driving_resources_non_decreasing` on the driven frames) is proven
// LOAD-BEARING by N6 above, which vetoes through that same function (branches a/b/d share it).
// The branch-(a)/(b) sign-checks are fail-closed DEFENSIVE guards — live-unreachable in TODAY's
// buyback-recast mechanism, NOT dead code; they fire the moment a future recast mechanism or a
// per-recast energy/player-counter-drain card makes them reachable. Add a live fixture then.

// ---------------------------------------------------------------------------
// Stage 1 — ShortcutDecisionSchema on the LoopShortcut offer (T1/T2/T4/T6).
// ---------------------------------------------------------------------------

/// T1 ⭐: the object-growth (convoke-recast) offer carries exactly ONE `ConvokeTaps`
/// decision-point whose `tappable` is the LIVE offer-time `is_convoke_eligible(P0)` set, and an
/// optional-loop `Fixed(1)` iteration seed. Board-derivation (hostile): the creature TAPPED to
/// pay convoke during the real cast is EXCLUDED; an untapped controlled creature is INCLUDED — a
/// constant/hard-coded set could not track which creature was spent. Revert-probe: a builder
/// that dropped the ConvokeTaps pin (empty points) or hard-coded the set fails these.
#[test]
fn object_growth_offer_schema_has_live_convoke_taps() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    let WaitingFor::LoopShortcut { schema, .. } = outcome.final_waiting_for() else {
        panic!(
            "expected a LoopShortcut offer, got {:?}",
            outcome.final_waiting_for()
        );
    };

    // Exactly one open decision-point, and it is the convoke tap set (Sprout Swarm has Convoke).
    assert_eq!(
        schema.points.len(),
        1,
        "one open decision-point (convoke), got {:?}",
        schema.points
    );
    let DecisionPointKind::ConvokeTaps { tappable } = &schema.points[0].kind else {
        panic!(
            "expected a ConvokeTaps decision-point, got {:?}",
            schema.points[0].kind
        );
    };
    // Optional Advantage loop ⇒ Fixed(1) frontend count seed (not a determinate drain).
    assert_eq!(schema.iteration_count, IterationCount::Fixed(1));

    // The tappable set is LIVE-derived from the offer-time board: exactly the untapped creatures
    // P0 controls (== is_convoke_eligible(P0)), compared as a set.
    let expected: std::collections::BTreeSet<ObjectId> = outcome
        .state()
        .objects
        .values()
        .filter(|o| o.is_convoke_eligible(P0))
        .map(|o| o.id)
        .collect();
    let got: std::collections::BTreeSet<ObjectId> = tappable.iter().copied().collect();
    assert_eq!(
        got, expected,
        "tappable must equal the live is_convoke_eligible(P0) set"
    );
    assert!(
        !expected.is_empty(),
        "reach-guard: the convoke set is non-empty"
    );

    // Board-derivation (hostile): fodder[0] was TAPPED to pay convoke during the real cast, so it
    // is EXCLUDED from the offer-time tap set, while an untapped controlled creature is INCLUDED.
    assert!(
        outcome.state().objects.get(&fodder[0]).unwrap().tapped,
        "reach-guard: fodder[0] is tapped from paying convoke"
    );
    assert!(
        !got.contains(&fodder[0]),
        "the tapped convoke payer is excluded from the live tap set"
    );
    assert!(
        got.contains(&fodder[1]),
        "an untapped controlled creature is included"
    );

    // The point's slot binds the recast card's CR 400.7 AllCopies identity.
    let ctx = outcome.state().last_loop_action_sequence.first().unwrap();
    assert_eq!(
        schema.points[0].slot.source,
        YieldTarget::AllCopies {
            card_id: ctx.card_id,
            trigger_description: None,
        },
        "the convoke slot binds the recast card identity"
    );
    let _ = sprout;
}

/// T2: a non-targeted drain offer reifies NO per-iteration decision-points (empty schema), and a
/// determinate CR 704.5a lethal drain seeds `UntilLethal`. T1's non-empty ConvokeTaps set is the
/// reach-guard against "the schema is always empty".
#[test]
fn drain_offer_schema_is_empty_until_lethal() {
    let (runner, _l0, _cleric) = reach_2p_optional_drain_offer();
    let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a LoopShortcut offer, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        schema.points.is_empty(),
        "a non-targeted drain reifies no decision-points, got {:?}",
        schema.points
    );
    assert_eq!(
        schema.iteration_count,
        IterationCount::UntilLethal,
        "a determinate lethal drain repeats UntilLethal"
    );
}

/// T4 ⭐ (SECURITY): a `LoopShortcut` schema's hidden-info legal targets are redacted per-viewer.
/// The controller (P0) keeps every legal target; a non-controller (P2) loses ONLY the target
/// that is a hidden card in an opponent's hand, retaining the public `Player` and battlefield
/// object targets. Two-directional: the controller-retains half is the reach-guard against an
/// unconditional drop. Revert-probe: deleting the `WaitingFor::LoopShortcut` block in
/// `filter_state_for_viewer` makes P2's view retain the hidden hand card ⇒ this test fails (leak).
#[test]
fn loop_shortcut_schema_redacts_hidden_targets_for_non_controller() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hidden_hand = scenario.add_bolt_to_hand(P1); // a hidden card in P1's hand
    let battlefield = scenario.add_creature(P0, "Test Ogre", 3, 3).id(); // public battlefield object
    let mut runner = scenario.build();

    let slot = DecisionSlot {
        source: YieldTarget::ThisObject {
            source_id: ObjectId(999),
            incarnation: None,
            trigger_description: None,
        },
        index: 0,
    };
    let schema = ShortcutDecisionSchema {
        iteration_count: IterationCount::UntilLethal,
        // No narrowed CR 732.2a bound — the global cap, as every offer states today.
        max_iterations: ShortcutDecisionSchema::default().max_iterations,
        points: vec![DecisionPoint {
            slot,
            kind: DecisionPointKind::Targets {
                legal_targets: vec![
                    TargetRef::Object(hidden_hand),
                    TargetRef::Player(P1),
                    TargetRef::Object(battlefield),
                ],
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };
    let cert = LoopCertificate {
        unbounded: vec![],
        win_kind: WinKind::LethalDamage,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: cert,
        schema,
    };

    let targets_of = |wf: &WaitingFor| -> Vec<TargetRef> {
        let WaitingFor::LoopShortcut { schema, .. } = wf else {
            panic!("expected LoopShortcut, got {wf:?}");
        };
        let DecisionPointKind::Targets { legal_targets, .. } = &schema.points[0].kind else {
            panic!("expected a Targets point, got {:?}", schema.points[0].kind);
        };
        legal_targets.clone()
    };

    // Controller P0 (reach-guard): keeps ALL three legal targets — the redaction is
    // viewer-scoped, not an unconditional drop.
    let p0_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
    let p0_targets = targets_of(&p0_view.waiting_for);
    assert_eq!(p0_targets.len(), 3, "controller keeps all legal targets");
    assert!(p0_targets.contains(&TargetRef::Object(hidden_hand)));
    assert!(p0_targets.contains(&TargetRef::Player(P1)));
    assert!(p0_targets.contains(&TargetRef::Object(battlefield)));

    // Non-controller P2: drops ONLY the hidden hand Object; retains the public Player + the
    // public battlefield Object.
    let p2_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P2);
    let p2_targets = targets_of(&p2_view.waiting_for);
    assert!(
        !p2_targets.contains(&TargetRef::Object(hidden_hand)),
        "leak: a non-controller must NOT see the hidden hand card as a legal target: {p2_targets:?}"
    );
    assert!(
        p2_targets.contains(&TargetRef::Player(P1)),
        "the public Player target is retained: {p2_targets:?}"
    );
    assert!(
        p2_targets.contains(&TargetRef::Object(battlefield)),
        "the public battlefield object target is retained: {p2_targets:?}"
    );
    assert_eq!(
        p2_targets.len(),
        2,
        "exactly the one hidden target is dropped"
    );
}

/// T6 (serde): the schema rides the `WaitingFor::LoopShortcut` serialization as `data.schema`
/// (tag/content) and round-trips equal — the FE contract that lets the frontend read the offer's
/// decision schema off the wire without any engine-side special casing.
#[test]
fn loop_shortcut_serializes_schema_under_data() {
    let (runner, _l0, _cleric) = reach_2p_optional_drain_offer();
    let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a LoopShortcut offer, got {:?}",
            runner.state().waiting_for
        );
    };
    let v = serde_json::to_value(&runner.state().waiting_for).expect("serialize WaitingFor");
    assert!(
        v["data"]["schema"].is_object(),
        "WaitingFor::LoopShortcut must serialize data.schema, got {v}"
    );
    let schema_back: ShortcutDecisionSchema =
        serde_json::from_value(v["data"]["schema"].clone()).expect("deserialize schema");
    assert_eq!(&schema_back, schema, "the schema round-trips off the wire");
}

/// T-concede-winner — the `predicted_winner` conjunct of the `apply_confirmed_shortcut` liveness
/// guard (`engine.rs:864-878`). The latched PREDICTED WINNER (not the proposer) concedes DURING the
/// open CR 732.2b APNAP window. `GameAction::Concede` bypasses the `WaitingFor` dispatch, so the
/// offer survives with a departed winner latched in `proposal.predicted_winner`. On the last living
/// opponent's Accept, the guard must REFUSE to act on the stale proposal (CR 104.3a: the winner has
/// left and lost; CR 104.2a: a departed player cannot be crowned; CR 800.4a: their objects are gone,
/// so the sequence they were predicted to win is not the sequence on the board) and hand priority to
/// a living seat — WITHOUT driving a single cycle.
///
/// # ⚠️ DO NOT "SIMPLIFY" THE LIFE ASSERTIONS. They are the only ones with teeth.
///
/// `waiting_for` is `Priority { P0 }` in BOTH arms of the revert-probe, and `GameOver` is reached in
/// NEITHER (`Fixed(n)` materializes cycles; it does not crown). Therefore
/// `assert!(!matches!(wf, GameOver{..}))` and "priority went to a living seat" PASS WITH THE GUARD
/// DELETED — they are CR 800.4a post-remedy INVARIANTS (the remedy must leave a valid state), NOT
/// discriminators. **The only assertion with teeth is the board: `life(P0)` / `life(P1)` unchanged.**
/// Measured revert-probe: guard present ⇒ (998, 998, 1000); guard deleted ⇒ (995, 995, 1000).
///
/// # Why `Fixed(3)` and not `UntilLethal`
///
/// `apply_until_lethal_shortcut` re-derives the winner through `live_mandatory_loop_winner`, whose
/// `!p.is_eliminated` living-filter ALREADY refuses to name a departed player — so on that path the
/// conjunct is redundant defence-in-depth and any test would be vacuous.
/// `materialize_fixed_shortcut` NEVER consults `predicted_winner` and COMMITS each driven cycle, so
/// this conjunct is the ONLY thing between a departed winner and 3 committed loop cycles.
///
/// `Fixed(n)` is reachable via the public `GameAction` surface (UI, scripted client, server payload
/// surface): `handle_declare_shortcut` moves `count` into the proposal with zero validation; the
/// fail-closed firewall validates only `template` pins and is skipped entirely when `template` is
/// `None`. It is NOT emitted by the AI's own candidate generator, which hardcodes `UntilLethal`.
///
/// # Why this test scripts `DeclareShortcut` directly instead of routing through the AI
///
/// This same board is ALSO a firing case for `phase_ai::policies::loop_shortcut::LoopShortcutPolicy`
/// (proposer P0 is a faller; the winner P2 != proposer ⇒ the policy REJECTS `DeclareShortcut`). A
/// future reader must not "fix" this test by routing it through the AI picker — the AI will now
/// correctly refuse to declare, and the test would silently stop reaching the engine seam.
///
/// REVERT-PROBE: delete `|| proposal.predicted_winner.is_some_and(|winner| !is_alive(state, winner))`
/// from `apply_confirmed_shortcut`. The proposer P0 is alive, so the guard no longer fires;
/// `materialize_fixed_shortcut` drives and commits 3 cycles of the still-live plague engine. The
/// `life(P1) == p1_before` assertion FAILS with left = 995, right = 998.
#[test]
fn predicted_winner_concede_mid_apnap_does_not_drive() {
    let (mut runner, kickoff) = setup_3p_bystander_winner(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 600);

    // ── REACH-GUARDS: the offer is ENGINE-LATCHED, not injected ────────────────────────────────
    // This pair is what proves the fixture is real. If the engine ever stops naming a
    // non-owner bystander as the winner, this test must FAIL LOUDLY, not silently degrade.
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ..
    } = wf
    else {
        panic!("the engine must OFFER on this board (optional loop), got {wf:?}");
    };
    assert_eq!(proposer, P0, "the priority holder proposes (CR 732.2a)");
    assert_eq!(
        predicted_winner,
        Some(P2),
        "the engine must latch the life-loss-immune BYSTANDER as winner — a player who controls \
         no loop enabler and is not the proposer (CR 732.2a: the shortcut's ending point need not \
         be the proposer)"
    );
    assert!(
        life(&runner, P0) < 1000 && life(&runner, P1) < 1000,
        "both fallers have bled"
    );

    // P0 declares `Fixed(3)` — the count whose apply path never re-consults `predicted_winner`.
    // `template: None` skips `handle_declare_shortcut`'s pin firewall entirely.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("P0 declares Fixed(3)");

    // CR 732.2b: the window opens in turn order starting AFTER the proposer ⇒ P1, then P2.
    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Declare must open the APNAP window, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "window opens on P1");
    assert_eq!(
        remaining_players,
        vec![P2],
        "P2 (the latched winner) is queued behind P1"
    );

    // CR 104.3a: the latched PREDICTED WINNER concedes mid-window. The acting responder (P1) is
    // alive, so the elimination self-heal leaves the stale offer standing.
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 (the predicted winner) concedes");
    assert!(is_eliminated(&runner, P2), "P2 has left the game");
    assert!(
        !is_eliminated(&runner, P0) && !is_eliminated(&runner, P1),
        "P0 and P1 remain — a living seat exists to receive priority (CR 800.4a)"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::RespondToShortcut { player, .. } if player == P1),
        "the offer survives the conceder (acting P1 is alive), got {:?}",
        runner.state().waiting_for
    );

    let p0_before = life(&runner, P0);
    let p1_before = life(&runner, P1);

    // The last living opponent accepts ⇒ CR 732.2c ⇒ `apply_confirmed_shortcut` with a STALE
    // `predicted_winner` (P2, departed) and a LIVING proposer (P0).
    accept_all_opponents(&mut runner);

    // ── (b) THE DISCRIMINATOR — the board is untouched. DO NOT DELETE. ─────────────────────────
    assert_eq!(
        life(&runner, P1),
        p1_before,
        "guard must REFUSE to drive: P1's life must be untouched by the refused shortcut"
    );
    assert_eq!(
        life(&runner, P0),
        p0_before,
        "…and so must the proposer's (Fixed(n) COMMITS every cycle it drives)"
    );

    // ── POST-REMEDY INVARIANTS (necessary, NOT discriminating — see the doc comment) ───────────
    // (a) CR 104.2a: no crown, for anyone.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "a stale proposal whose predicted winner has left must not end the game, got {:?}",
        runner.state().waiting_for
    );
    // (c) CR 800.4a: priority lands on a LIVING seat.
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => assert!(
            !is_eliminated(&runner, player),
            "CR 800.4a: priority must never land on a departed seat"
        ),
        ref other => panic!("the liveness guard hands priority back (manual play), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// BB-FU10 T16 — a battlefield-entry-LEDGER observer VETOES the object-growth
// offer. This is the ruling's disclosed, sound post-Step-0c behaviour, asserted
// as such so nobody "fixes" the test by deleting it.
// ---------------------------------------------------------------------------

/// Park Heights Pegasus, verbatim (Scryfall / MTGJSON `AtomicCards.json`). Its
/// trigger `execute` body carries the CR 608.2i
/// `QuantityRef::BattlefieldEntriesThisTurn` read, which
/// `fire_time_conditions_read_growing_class` block (1) scans at the
/// `ability_definition_reads_sibling_mutable_for_loop` call site.
const PARK_HEIGHTS_PEGASUS_ORACLE: &str = "Flying, trample\nWhenever this creature deals combat damage to a player, draw a card if you had two or more creatures enter the battlefield under your control this turn.";

/// ANTI-VACUITY CONTROL: the same board shape with a trigger that reads NOTHING
/// board-mutable. Granted in BOTH builds, which is what proves the veto below
/// comes from the ledger clause and not from the bystander's mere presence.
const PLAIN_DRAW_TRIGGER_ORACLE: &str =
    "Flying, trample\nWhenever this creature deals combat damage to a player, draw a card.";

/// The passing 51st Sprout Swarm / Witherbloom object-growth row plus exactly ONE
/// extra battlefield permanent, controlled by `bystander_controller`, carrying
/// `bystander_oracle`. Returns the final `WaitingFor` plus the bystander's id, so the
/// caller can reach-guard its zone.
///
/// `phase` parameterises the loop window's step (CR 500.1 / CR 506.1), which is what
/// the CR 510.2 phase-unreachability rows key on; `bystander_controller` parameterises
/// the observer's controller, which is what the CR 117.1b sole-driver rows key on. Both
/// axes exist so a row can move exactly ONE variable against its own control.
fn object_growth_with_bystander_at(
    phase: Phase,
    bystander_controller: PlayerId,
    bystander_oracle: &str,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    let bystander = scenario
        .add_creature_from_oracle(
            bystander_controller,
            "BBFU10 Bystander",
            2,
            2,
            bystander_oracle,
        )
        .id();
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    // The cast pipeline settles the loop verdict into `state().waiting_for`, which
    // is what the caller reads (the borrowed `CastOutcome` is dropped here).
    runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    (runner, bystander)
}

/// The shipped two-call-site shape: P0's own bystander, precombat main.
fn object_growth_with_bystander(bystander_oracle: &str) -> (GameRunner, ObjectId) {
    object_growth_with_bystander_at(Phase::PreCombatMain, P0, bystander_oracle)
}

/// T16 (BB-FU10 RULING deliverable). With Step 0c applied, a shipped
/// battlefield-entry-ledger observer anywhere on a functioning battlefield
/// SUPPRESSES a CR 732.2a object-growth offer that fires without it.
///
/// This asserts the SUPPRESSION as the sound behaviour. Per the plan's §0.5
/// ruling, the engine already classifies `battlefield_entries_this_turn` as a
/// journal a loop pumps (`project_out_resources` clears it), so `sibling: false`
/// let the firewall hand out a false ∞ certificate while a live observer read the
/// growing class — the one error direction `ability_scan`'s ADD-1 contract
/// forbids.
///
/// **`BB-FU10-N` SHIPPED IN THIS COMMIT.** Assertion (1) is now an **OFFER**. The
/// flip's mechanism is **X2 phase/step unreachability** (CR 510.2 / CR 506.1;
/// CR 500.1 for the phase list), *not* filter-matching: Park Heights Pegasus's
/// ledger filter is `Typed{Creature}`, which genuinely **does** match a Saproling
/// token, so gating the veto on filter-match leaves this card vetoed — measured by
/// rebuilding the same board with a `Typed{Artifact}` ledger filter, which still
/// vetoed at BASE. The card's trigger is `damage_kind: CombatOnly` and the loop
/// window is `PreCombatMain`, so the observer cannot fire inside the window. The
/// shallow filter-match narrowing now also ships (a `QuantityCheck`-shaped ledger read
/// sitting directly in a block-(1) trigger's `execute.condition`, proven sole-source by
/// single-field clone-and-rescan); rows `K4-N1`/`K4-N2` are its matched pair. Measured on
/// the current card pool that shape matches exactly ONE printed card — this one — which
/// it correctly REFUSES, because `Typed{Creature}` genuinely counts a Saproling creature
/// token. Everything the shallow form cannot reach — `trigger.condition` observers (21
/// cards), statics (16), abilities (13), `casting_options` (4), replacements (1), compound
/// conditions, rhs-position reads, blocks (2)/(3)/(5b) — remains **`BB-FU10-N2`**.
///
/// REVERT-PROBE: delete X2's `continue` in
/// `fire_time_conditions_read_growing_class_scoped` block (1) ⇒ this row returns to
/// a veto and FAILS. The (2) control is granted in BOTH builds. **Second,
/// independent probe:** make `trigger_event_unreachable_in_phase` return `false`
/// unconditionally ⇒ the same failure ⇒ the *predicate*, not the plumbing, carries
/// the flip.
#[test]
fn object_growth_phase_unreachable_ledger_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    // (2) ANTI-VACUITY CONTROL first: an otherwise byte-identical board whose
    // bystander reads nothing board-mutable still gets the offer.
    let (control_runner, control_bystander) =
        object_growth_with_bystander(PLAIN_DRAW_TRIGGER_ORACLE);
    match &control_runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "(2) control: the detected loop's unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "(2) anti-vacuity control: a plain draw-trigger bystander must NOT suppress \
             the offer, got {other:?}"
        ),
    }
    assert_eq!(
        control_runner.state().objects[&control_bystander].zone,
        Zone::Battlefield,
        "(3) control reach-guard: the bystander is a functioning battlefield permanent"
    );

    // The subject: the SAME board with Park Heights Pegasus instead.
    let (runner, bystander) = object_growth_with_bystander(PARK_HEIGHTS_PEGASUS_ORACLE);

    // (3) reach-guard — block (1) hard-skips non-battlefield zones, so the observer
    // must actually be on the battlefield, and must carry exactly one trigger.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "(3) reach-guard: the ledger observer must be a functioning battlefield permanent"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "(3) reach-guard: exactly one trigger definition carries the ledger read"
    );

    // (1) THE OFFER — X2's phase-unreachability relief (CR 510.2 / CR 506.1).
    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            certificate,
            predicted_winner,
            ..
        } => {
            assert!(
                certificate.unbounded.contains(&ResourceAxis::TokensCreated),
                "(1) the detected loop's unbounded axis must be TokensCreated, got {:?}",
                certificate.unbounded
            );
            assert_eq!(
                *predicted_winner, None,
                "(1) this is an Advantage offer, not a predicted win"
            );
        }
        other => panic!(
            "(1) CR 510.2 / CR 506.1: Park Heights Pegasus's combat-damage trigger cannot \
             fire inside a PreCombatMain loop window, so it must NOT suppress the \
             CR 732.2a object-growth offer; got {other:?}"
        ),
    }
}

/// HF-X2-a (hostile fixture for X2-1) — the SAME Park Heights Pegasus board with the
/// loop window at `Phase::CombatDamage`. There the observer's combat-damage event IS
/// reachable (CR 510.2), `trigger_event_unreachable_in_phase` returns `false`, and the
/// conservative veto is preserved. Paired with X2-1 this is a matched pair moving
/// exactly ONE variable: the window's phase.
///
/// The control half proves the board still detects a loop at this step, so the subject
/// half's no-offer is a real veto and not a dead harness.
///
/// REVERT-PROBE: drop the `phase != Phase::CombatDamage` conjunct from the damage arm
/// ⇒ the subject half flips to an offer ⇒ FAILS.
#[test]
fn combat_damage_step_ledger_observer_still_suppresses_offer() {
    use engine::types::zones::Zone;

    // Control: the plain-draw bystander on the same board at the same step.
    let (control_runner, _) =
        object_growth_with_bystander_at(Phase::CombatDamage, P0, PLAIN_DRAW_TRIGGER_ORACLE);
    assert!(
        matches!(
            control_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "HF-X2-a REACH-GUARD: the loop must still be detected and offered at \
         Phase::CombatDamage, else the subject half below proves nothing. \
         (Pre-registered STOP branch: if this fails, report the rejecting gate and \
         DROP HF-X2-a — X2-4a/X2-4b keep the phase-keying proof.) got {:?}",
        control_runner.state().waiting_for
    );

    // Subject: Pegasus, whose CombatOnly trigger IS reachable in this step.
    let (runner, bystander) =
        object_growth_with_bystander_at(Phase::CombatDamage, P0, PARK_HEIGHTS_PEGASUS_ORACLE);

    // (3) reach-guards — block (2) hard-skips non-battlefield zones, and this row's claim is
    // about ONE named TRIGGER surface: without these the veto could arrive from a surface the
    // row does not name (wrong-attribution vacuity).
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (2) hard-skips non-battlefield zones, so a veto from this \
         bystander would not be attributable to it at all"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: this row's claim is about ONE named trigger surface; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}, so a veto here would not be attributable to \
         the trigger",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "CR 510.2: in the combat damage step the observer's event IS reachable, so the \
         veto must be preserved; got {:?}",
        runner.state().waiting_for
    );
}

/// Smuggler's Share, verbatim (Scryfall `cards/named?exact=`), behind the harness's
/// shared `"Flying, trample\n"` keyword prefix so subject and control differ ONLY in
/// the ledger clause. Its trigger is `TriggerMode::Phase` with `phase: End`.
const SMUGGLERS_SHARE_ORACLE: &str = "Flying, trample\nAt the beginning of each end step, draw a card for each opponent who drew two or more cards this turn, then create a Treasure token for each opponent who had two or more lands enter the battlefield under their control this turn.";

/// X2-2 — a SECOND trigger mode reaches the same relief. Smuggler's Share's
/// `{Phase, End}` observer cannot fire inside a `PreCombatMain` loop window
/// (CR 500.1 / CR 506.1), so it must not suppress the CR 732.2a offer.
///
/// REVERT-PROBE: delete X2's `TriggerMode::Phase` arm (or widen it to `p == phase`)
/// ⇒ the veto returns ⇒ FAILS.
#[test]
fn smugglers_share_end_step_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    // (2) ANTI-VACUITY CONTROL, granted in BOTH builds.
    let (control_runner, _) = object_growth_with_bystander(PLAIN_DRAW_TRIGGER_ORACLE);
    assert!(
        matches!(
            control_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "(2) control: a plain draw-trigger bystander must not suppress the offer"
    );

    let (runner, bystander) = object_growth_with_bystander(SMUGGLERS_SHARE_ORACLE);

    // (3) reach-guards — block (1) hard-skips non-battlefield zones.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "(3) reach-guard: exactly one trigger definition carries the ledger read"
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "(1) unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "(1) CR 500.1 / CR 506.1: an end-step observer cannot fire inside a \
             precombat-main loop window, so it must not suppress the offer; got {other:?}"
        ),
    }
}

/// HF-X2-c (hostile fixture for X2-2) — the SAME Smuggler's Share board with the loop
/// window at `Phase::End`. Now `def.phase == Some(End) == phase`, the ⛔ PINNED strict
/// inequality returns `false`, and the veto is preserved. That refusal is a SOUNDNESS
/// bound, not conservatism for its own sake: per CR 117.3a the end-step ability is put
/// on the stack BEFORE the priority at which CR 732.2a lets a shortcut be proposed, and
/// CR 608.2h determines its information at resolution — inside the window.
///
/// REVERT-PROBE: widen the `Phase` arm to `def.phase.is_some()` ⇒ this flips to an
/// offer ⇒ FAILS.
#[test]
fn end_step_window_end_step_observer_still_suppresses_offer() {
    use engine::types::zones::Zone;

    let (control_runner, _) =
        object_growth_with_bystander_at(Phase::End, P0, PLAIN_DRAW_TRIGGER_ORACLE);
    assert!(
        matches!(
            control_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "HF-X2-c REACH-GUARD: the loop must still be detected and offered at Phase::End, \
         else the subject half proves nothing. (Pre-registered STOP branch: if this \
         fails, report the rejecting gate and DROP HF-X2-c.) got {:?}",
        control_runner.state().waiting_for
    );

    let (runner, bystander) =
        object_growth_with_bystander_at(Phase::End, P0, SMUGGLERS_SHARE_ORACLE);

    // (3) reach-guards — see the sibling row: the veto must be attributable to the ONE
    // named trigger surface, not to some other surface on this bystander.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (2) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: this row's claim is about ONE named trigger surface; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "CR 117.3a + CR 608.2h: an end-step observer in an END-STEP window keeps its \
         veto — the strict-inequality pin; got {:?}",
        runner.state().waiting_for
    );
}

/// The Prydwen, Steel Flagship, verbatim (Scryfall `cards/named?exact=`), behind the
/// harness's shared keyword prefix. Its ETB matcher is `nontoken artifact you control`,
/// which is triple-disjoint from a P0 Saproling creature TOKEN.
const PRYDWEN_ORACLE: &str = "Flying, trample\nFlying\nWhenever another nontoken artifact you control enters, create a 2/2 white Human Knight creature token with \"This token gets +2/+2 as long as an artifact entered the battlefield under your control this turn.\"\nCrew 2";

/// The SAME card with its ETB matcher widened from `nontoken artifact` to `creature`,
/// which genuinely DOES match the loop's Saproling fodder.
const PRYDWEN_BROAD_ORACLE: &str = "Flying, trample\nFlying\nWhenever another creature you control enters, create a 2/2 white Human Knight creature token with \"This token gets +2/+2 as long as an artifact entered the battlefield under your control this turn.\"\nCrew 2";

/// K3-1 + HF-K3 — REGRESSION LOCK on the already-shipped
/// `etb_observer_provably_excludes_class` narrowing (no code changes in this commit).
/// A matched pair one matcher-noun apart: the disjoint `nontoken artifact` matcher is
/// skipped (CR 603.6a) and the offer forms; widening it to `creature` makes it
/// genuinely match the Saproling fodder and the veto returns.
///
/// REVERT-PROBE (K3-1): delete the `etb_observer_provably_excludes_class` call in
/// `fire_time_conditions_read_growing_class_scoped` block (1) ⇒ the offer disappears ⇒
/// FAILS. It is NOT the `ability_scan` `sibling` flip — measured, that does not flip
/// this row.
#[test]
fn prydwen_artifact_matcher_bystander_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let (runner, bystander) = object_growth_with_bystander(PRYDWEN_ORACLE);

    // (3) reach-guards, ALL BEFORE the offer match. On a positive (offer-forming) row a
    // parse failure yields NO observer at all, which would make the offer trivially green —
    // these guards are what make that vacuity mode loud. `Crew` is a keyword, not an
    // `abilities[]` entry, so the ONE surface here is the ETB trigger.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (2) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: the ETB observer must have PARSED — a misparse leaves zero trigger \
         defs and the offer below forms for the wrong reason; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "K3-1: unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "K3-1 CR 603.6a: an ETB matcher provably disjoint from the fodder class must \
             not suppress the offer; got {other:?}"
        ),
    }

    // HF-K3: the genuinely-matching sibling keeps its veto.
    let (broad_runner, broad_bystander) = object_growth_with_bystander(PRYDWEN_BROAD_ORACLE);

    // (3) reach-guards on the BROAD half — this is a veto row, so the veto must be
    // attributable to the ONE named trigger surface and not to any other.
    let broad_obj = &broad_runner.state().objects[&broad_bystander];
    assert_eq!(
        broad_obj.zone,
        Zone::Battlefield,
        "reach-guard: block (2) hard-skips non-battlefield zones"
    );
    assert_eq!(
        broad_obj.trigger_definitions.len(),
        1,
        "reach-guard: this row's claim is about ONE named trigger surface; got {}",
        broad_obj.trigger_definitions.len()
    );
    assert!(
        broad_obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        broad_obj.abilities.len(),
        broad_obj
            .abilities
            .iter()
            .map(|a| a.kind)
            .collect::<Vec<_>>(),
    );

    assert!(
        !matches!(
            broad_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "HF-K3: an ETB matcher that DOES match the Saproling fodder must keep vetoing; \
         got {:?}",
        broad_runner.state().waiting_for
    );
}

/// A non-mana activated ability whose body reads a live board aggregate
/// (`QuantityRef::ObjectCount`). `ability_scan`'s `ObjectCount` arm self-asserts
/// `sibling: true` BEFORE it inspects the filter, so this surface vetoes regardless of
/// whose creatures the filter names — which is exactly why CR 117.1b (whose PRIORITY
/// the window belongs to), not the filter, is X1's relief axis.
const AGGREGATE_ACTIVATED_ORACLE: &str =
    "Flying, trample\n{2}: Draw a card for each creature you control.";

/// Circle of Dreams Druid's mana ability, verbatim (Scryfall `cards/named?exact=`),
/// behind the shared keyword prefix — the same `ObjectCount` aggregate read on a MANA
/// ability, which CR 605.3a keeps activatable without priority.
const AGGREGATE_MANA_ORACLE: &str = "Flying, trample\n{T}: Add {G} for each creature you control.";

/// A SECOND, non-activated class-reading surface on the same object: a trigger whose
/// body carries the same `ObjectCount` aggregate. `TriggerMode::Attacks` is
/// unclassifiable by phase, so X2 cannot relieve it either.
const AGGREGATE_TWO_SURFACE_ORACLE: &str = "Flying, trample\n{2}: Draw a card for each creature you control.\nWhenever this creature attacks, draw a card for each creature you control.";

/// X1-2 — CR 117.1b's relief is keyed on the OBSERVER'S CONTROLLER, and the matched
/// pair moves exactly that one variable. The DRIVER'S OWN class-reading activated
/// ability keeps vetoing (the driver holds priority inside its own shortcut and can
/// activate it); the identical ability under an OPPONENT is relieved.
///
/// REVERT-PROBE: invert the `obj.controller != driver` comparison ⇒ the two halves swap
/// ⇒ BOTH assertions FAIL.
#[test]
fn driver_own_activated_ability_still_vetoes() {
    use engine::types::ability::AbilityKind;
    use engine::types::zones::Zone;

    // PAIRED POSITIVE first: the same ability under an OPPONENT is relieved.
    let (foreign_runner, _) =
        object_growth_with_bystander_at(Phase::PreCombatMain, P1, AGGREGATE_ACTIVATED_ORACLE);
    assert!(
        matches!(
            foreign_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "X1 PAIRED POSITIVE (CR 117.1b + CR 732.2c): no player but the sole driver \
         receives priority inside the taken shortcut, so an OPPONENT's activated \
         ability cannot read the growing class and must not suppress the offer; got {:?}",
        foreign_runner.state().waiting_for
    );

    // SUBJECT: byte-identical board, ability under the DRIVER.
    let (own_runner, bystander) =
        object_growth_with_bystander_at(Phase::PreCombatMain, P0, AGGREGATE_ACTIVATED_ORACLE);

    // (3) reach-guards — the veto must come from the ONE named ACTIVATED-ability surface.
    // `kind == Activated` is what item A makes load-bearing on the very relief this row
    // exercises, and `trigger_definitions.is_empty()` keeps block (1) silent so the verdict
    // is attributable to block (2).
    let obj = &own_runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (2) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.abilities.len(),
        1,
        "reach-guard: exactly one ability surface; got {:?}",
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>()
    );
    assert_eq!(
        obj.abilities[0].kind,
        AbilityKind::Activated,
        "reach-guard: X1's relief is stated for ACTIVATED abilities only, so this row's \
         subject must BE one; got {:?}",
        obj.abilities[0].kind
    );
    assert!(
        obj.trigger_definitions.is_empty(),
        "reach-guard: block (1) must be silent, so the verdict is attributable to block \
         (2); got {} trigger def(s)",
        obj.trigger_definitions.len()
    );

    assert!(
        !matches!(
            own_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "X1-2: the DRIVER's own class-reading activated ability must keep vetoing — the \
         driver does hold priority inside its own window; got {:?}",
        own_runner.state().waiting_for
    );
}

/// HF-X1-a — CR 605.3a BOUNDS X1. A mana ability is activatable outside the priority
/// rule (while another player is casting a spell or activating an ability), so an
/// OPPONENT's class-reading MANA ability is NOT relieved and keeps vetoing. The paired
/// positive is the identical aggregate read on a NON-mana ability under the same
/// opponent, which IS relieved — so the only variable is `is_mana_ability`.
///
/// REVERT-PROBE: delete the `!is_mana_ability(..)` conjunct ⇒ the mana half is relieved
/// ⇒ FAILS.
#[test]
fn foreign_mana_ability_still_vetoes() {
    use engine::types::ability::AbilityKind;
    use engine::types::zones::Zone;

    // PAIRED POSITIVE: the same aggregate read on a NON-mana ability, same controller.
    let (nonmana_runner, _) =
        object_growth_with_bystander_at(Phase::PreCombatMain, P1, AGGREGATE_ACTIVATED_ORACLE);
    assert!(
        matches!(
            nonmana_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "HF-X1-a PAIRED POSITIVE: an opponent's NON-mana activated ability is relieved"
    );

    let (mana_runner, bystander) =
        object_growth_with_bystander_at(Phase::PreCombatMain, P1, AGGREGATE_MANA_ORACLE);

    // (3) reach-guards. The row's WHOLE claim is the CR 605.3a mana carve-out, so nothing
    // short of proving the def IS a mana ability makes the veto attributable to it.
    let obj = &mana_runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (2) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.abilities.len(),
        1,
        "reach-guard: exactly one ability surface; got {:?}",
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>()
    );
    assert_eq!(
        obj.abilities[0].kind,
        AbilityKind::Activated,
        "reach-guard: a mana ability is an ACTIVATED ability; got {:?}",
        obj.abilities[0].kind
    );
    assert!(
        engine::game::mana_abilities::is_mana_ability(&obj.abilities[0]),
        "reach-guard: this row's entire claim is the CR 605.3a mana carve-out, so the def \
         must actually BE a mana ability — otherwise the veto is attributable to the \
         ordinary foreign-activated path and the row proves nothing"
    );
    assert!(
        obj.trigger_definitions.is_empty(),
        "reach-guard: block (1) must be silent, so the verdict is attributable to block \
         (2); got {} trigger def(s)",
        obj.trigger_definitions.len()
    );

    assert!(
        !matches!(
            mana_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "HF-X1-a CR 605.3a: a mana ability is activatable without priority, so an \
         opponent's class-reading MANA ability must keep vetoing; got {:?}",
        mana_runner.state().waiting_for
    );
}

/// NW-1' — X1's relief is PER-ABILITY and PER-SURFACE, never per-object. The two halves
/// carry the SAME opponent-controlled object; half B adds one extra surface (a trigger
/// whose body carries the same `ObjectCount` aggregate, scanned by block (1), which X1
/// does not touch and which `TriggerMode::Attacks` leaves unclassifiable for X2). Half A
/// offering is what proves half B's veto comes from the second surface and not from the
/// object's mere presence.
///
/// This is also the closure for the §I `ActivationRestriction` composition hazard at
/// the offer level: the firewall never reads `activation_restrictions`
/// (`ability_scan.rs:4238` destructures it as `_`), so a row keyed on that field would
/// be dominated. This row instead asserts the property the revert-probes actually flip.
///
/// REVERT-PROBE: widen X1's relief from the per-ability test to the whole object (skip
/// the object in block (2) AND block (1)) ⇒ half B flips to an offer ⇒ FAILS.
#[test]
fn foreign_object_second_surface_still_vetoes_after_x1() {
    use engine::types::ability::AbilityKind;
    use engine::types::zones::Zone;

    // half A: the relieved surface alone ⇒ offer.
    let (one_surface, _) =
        object_growth_with_bystander_at(Phase::PreCombatMain, P1, AGGREGATE_ACTIVATED_ORACLE);
    assert!(
        matches!(
            one_surface.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "NW-1' half A: with ONLY the foreign activated ability, X1 relieves and the \
         offer forms — so half B's veto is attributable to the added surface"
    );

    // half B: the same object plus one more class-reading surface ⇒ veto.
    let (two_surface, bystander) =
        object_growth_with_bystander_at(Phase::PreCombatMain, P1, AGGREGATE_TWO_SURFACE_ORACLE);
    let obj = &two_surface.state().objects[&bystander];
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "NW-1' reach-guard: the second surface really is a trigger definition"
    );
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "NW-1' reach-guard: block (2) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.abilities.len(),
        1,
        "NW-1' reach-guard: the FIRST surface is exactly one ability def; got {:?}",
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>()
    );
    assert_eq!(
        obj.abilities[0].kind,
        AbilityKind::Activated,
        "NW-1' reach-guard: half A's relieved surface is an ACTIVATED ability, so half B's \
         first surface must be the same one; got {:?}",
        obj.abilities[0].kind
    );
    assert!(
        !matches!(
            two_surface.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "NW-1': X1 relieves the ABILITY, not the OBJECT — another class-reading surface \
         on the same permanent must keep vetoing; got {:?}",
        two_surface.state().waiting_for
    );
}

// ===========================================================================
// PR-7 Phase 1b — CR 732.2a loop-detect ring retention across a FORCED
// pre-priority window and the action that answers it.
//
// Both rows LOAD a committed real 4-player dump through the production restore
// chokepoint (`PersistedGameState::Raw(..).into_game_state()`, the same path the
// server's `from_persisted` and WASM's `decode_restored_game_state` funnel
// through) and DRIVE through the public `apply()` boundary. Synthetic
// `GameScenario` boards are deliberately NOT used here: the property under test
// is an accumulation across dozens of real beats.
// ===========================================================================

fn gunzip_dump(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

fn restore_dump(json: &str) -> GameState {
    let envelope: serde_json::Value =
        serde_json::from_str(json).expect("dump envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("the real 4p gameState must restore through the persisted-state boundary")
        .into_game_state()
}

/// Opponents the ENGINE considers living. `Player::is_eliminated` is the authority the
/// CR 732.2a detector uses when it builds its `living` set — `eliminated_players` and
/// `life > 0` are not sufficient on their own, so this reads the field the detector reads.
fn engine_live_opponents(state: &GameState, of: PlayerId) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| p.id != of && !p.is_eliminated)
        .map(|p| p.id)
        .collect()
}

/// Actions a dump driver must never take: they end the game or bypass the reducer, and a
/// generic "first legal action" driver otherwise picks them and fakes a result.
fn dump_driver_forbids(a: &GameAction) -> bool {
    matches!(a, GameAction::Concede { .. } | GameAction::Debug(_))
}

/// The seat that can act on this beat plus its legal actions, read through the same
/// per-viewer enumerator the multiplayer transport uses. `WaitingFor::acting_player` is
/// the engine's own answer to "whose beat is this", so it is tried first; the all-seat
/// scan is the fallback and costs roughly 4x per beat.
fn dump_beat_actor(state: &GameState) -> Option<(PlayerId, Vec<GameAction>)> {
    if let Some(p) = state.waiting_for.acting_player() {
        let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, p);
        if !actions.is_empty() {
            return Some((p, actions));
        }
    }
    for p in state.players.iter().map(|p| p.id) {
        let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, p);
        if !actions.is_empty() {
            return Some((p, actions));
        }
    }
    None
}

/// One beat of the drain-loop drive policy: at `Priority` ALWAYS pass (the mandatory
/// triggers resolve and re-trigger — that IS the loop; casting here wanders off it), and
/// answer every other prompt, preferring a target choice aimed at `pin`.
///
/// Returns the beat's `GameEvent`s so a caller can key on what the beat actually DID
/// (`CombatDamageDealtToPlayer` / `DamageDealt` for the CR 510.2 rows) instead of
/// inferring it from phase and life deltas. Callers that only need liveness ignore it.
fn dump_drive_one_beat(
    state: &mut GameState,
    pin: Option<PlayerId>,
) -> Result<Vec<GameEvent>, String> {
    let Some((who, actions)) = dump_beat_actor(state) else {
        return Err(format!("no legal actor at {:?}", state.waiting_for));
    };
    let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .cloned()
    } else {
        pin.and_then(|t| {
            actions.iter().find(|a| {
                matches!(a, GameAction::SelectTargets { targets }
                    if targets.iter().any(|r| matches!(r, TargetRef::Player(p) if *p == t)))
            })
        })
        .or_else(|| {
            actions
                .iter()
                .find(|a| !matches!(a, GameAction::PassPriority) && !dump_driver_forbids(a))
        })
        .or_else(|| actions.iter().find(|a| !dump_driver_forbids(a)))
        .cloned()
    };
    let Some(action) = chosen else {
        return Err(format!("empty action list at {:?}", state.waiting_for));
    };
    apply(state, who, action.clone())
        .map(|r| r.events)
        .map_err(|e| format!("apply err ({action:?}): {e:?}"))
}

/// Phase 1b, BOTH clear sites. The CR 732.2a ring must survive (i) the forced
/// pre-priority window itself — the sampler's clear arm, which BASE took for every
/// window except `OrderTriggers` — and (ii) the action that ANSWERS that window, which
/// `apply_action`'s deliberate-break clear discarded unconditionally.
///
/// Fixture: `dellian_emblem_conqueror_4p.json.gz`, the real 4p Delianfel/Bloodthirsty
/// Conqueror drain (P0 69 / P1 12 / P2 13 / P3 28, all four living, stack 152, ring 0,
/// `loop_detection: Interactive`). It ships AT a `TriggerTargetSelection` window, which is
/// precisely the window class BASE wiped.
///
/// NON-VACUITY: the fixture drives hundreds of real beats (it is NOT a saved-offer board
/// that halts at beat 0), and the BASE measurement is the positive control that the
/// instrument CAN report a large ring — it reports 16 once only ONE opponent is left
/// alive, while measuring exactly 1 over the whole ≥2-living stretch. A `>= 5` assertion
/// over that same stretch cannot pass on a BASE tree.
///
/// REVERT-PROBES (MEASURED outcomes, not predicted ones):
/// ⓐ restore `apply_action`'s clear to its action-only form (drop the
///   `!state.waiting_for.is_forced_cascade_window()` conjunct) ⇒ (i) FAILS FIRST — and (ii)
///   and (iii) are never reached. The prediction that ⓐ would leave (i) passing was WRONG,
///   and the reason is the interlock between the two sites: with the answer clearing the
///   ring, the drive can never carry >= 2 frames INTO a forced window either, so the
///   sampler half has nothing left to retain. The two clear sites are therefore not
///   independently observable on this fixture — ⓐ still proves a one-site fix is inert,
///   just at (i) rather than at (ii).
/// ⓑ restore the sampler's `!matches!(wf, WaitingFor::OrderTriggers { .. })` arm ⇒ (i)
///   FAILS.
///
/// DISCRIMINANT GUARD on (ii): `apply_action` has a PRE-EXISTING action-side exemption for
/// `GameAction::OrderTriggers`, so if the window (ii) happens to catch were an
/// `OrderTriggers` window, (ii) would be satisfied without the new window-keyed conjunct
/// being consulted at all. The window's discriminant is therefore captured alongside
/// `(before, after)` and asserted to be something else — a fixture or engine change that
/// drifts (ii) onto an `OrderTriggers` window fails loudly instead of going quietly
/// vacuous.
#[test]
fn two_site_retention_survives_a_prompt_and_its_answer() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    ));
    let mut state = restore_dump(&json);

    // Reach guards on the loaded board — every assertion below is meaningless without them.
    assert!(
        state.loop_detection.samples(),
        "reach-guard: the dump must load with a SAMPLING loop-detection mode, else the \
         ring is never populated and every retention assertion is vacuous; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        engine_live_opponents(&state, P0).len(),
        3,
        "reach-guard: the dump must load with 3 living opponents"
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring — every frame below was accumulated \
         by this drive, not restored"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
        "reach-guard: the dump ships AT a TriggerTargetSelection window (CR 603.3d), the \
         window class BASE wiped; got {:?}",
        state.waiting_for
    );

    let pin = engine_live_opponents(&state, P0).first().copied();

    // (i) the `:3457` sampler half: a forced pre-priority window observed with an
    //     ALREADY-ACCUMULATED ring (>= 2 frames, so a single fresh sample cannot explain it).
    let mut prompt_ring: Option<usize> = None;
    // (ii) the `apply_action` half: the ring across the ANSWER to such a window, with the
    //      window itself so the row can prove it was not the pre-exempt `OrderTriggers`.
    let mut answer_ring: Option<(WaitingFor, usize, usize)> = None;
    // (iii) the ≥2-living stretch, where BASE measured a maximum of exactly 1.
    let mut max_ring_two_or_more_living = 0usize;

    for _ in 0..400 {
        if engine_live_opponents(&state, P0).len() >= 2 {
            max_ring_two_or_more_living =
                max_ring_two_or_more_living.max(state.loop_detect_ring.len());
        }
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        let forced = state.waiting_for.is_forced_cascade_window();
        let before = state.loop_detect_ring.len();
        let window = (forced && before >= 2).then(|| state.waiting_for.clone());
        if forced && before >= 2 {
            prompt_ring.get_or_insert(before);
        }
        if dump_drive_one_beat(&mut state, pin).is_err() {
            break;
        }
        if let (Some(window), None) = (window, answer_ring.as_ref()) {
            answer_ring = Some((window, before, state.loop_detect_ring.len()));
        }
        // Every assertion below is already satisfiable — stop driving. The drive is the
        // expensive part of this row (a per-beat legal-action enumeration on a 152-entry
        // stack), and continuing past the evidence buys nothing.
        if prompt_ring.is_some() && answer_ring.is_some() && max_ring_two_or_more_living >= 5 {
            break;
        }
    }

    let observed = prompt_ring.unwrap_or_else(|| {
        panic!(
            "(i) CR 603.3d: no forced pre-priority window was ever reached carrying an \
             accumulated ring of >= 2 frames. BASE behaviour (the sampler clearing at every \
             non-OrderTriggers window) is exactly this; max ring seen at >= 2 living was {max_ring_two_or_more_living}"
        )
    });
    assert!(
        observed >= 2,
        "(i) the ring must be RETAINED across the forced window itself"
    );

    let (window, before, after) = answer_ring.expect(
        "(ii) the drive must have applied the answer to a forced window that carried an \
         accumulated ring — otherwise the apply_action half is untested",
    );
    assert!(
        !matches!(window, WaitingFor::OrderTriggers { .. }),
        "(ii) DISCRIMINANT GUARD: `apply_action` already exempts `GameAction::OrderTriggers` \
         on the ACTION side, so an OrderTriggers window would satisfy the survival assertion \
         below without the window-keyed conjunct ever being consulted. The window measured \
         here must be one of the newly exempt classes; got {}",
        window.variant_name()
    );
    assert!(
        after >= before,
        "(ii) CR 603.3d + CR 732.2a: answering a forced pre-priority window is not a \
         deliberate break, so the accumulated ring must SURVIVE the answer; \
         ring went {before} -> {after}. Dropping the \
         `!state.waiting_for.is_forced_cascade_window()` conjunct at apply_action's clear \
         reproduces this failure — measured, it takes (i) down first, because the answer-side \
         clear also stops the ring ever reaching a forced window with >= 2 frames."
    );

    assert!(
        max_ring_two_or_more_living >= 5,
        "(iii) two full periods of the drain need 2k+1 = 5 retained frames while >= 2 \
         opponents are still alive; BASE measured exactly 1 over that stretch. Got \
         {max_ring_two_or_more_living}"
    );
}

/// Phase 1b crown-safety row. Retention only ADDS older frames to the ring, and
/// `find_live_loop_winner` scans every suffix, so a window that crowns today must still
/// crown after the exemption. Dump C is the population where that is measurable: it ships
/// with exactly ONE living opponent, which is the only shape `loop_check`'s
/// `nonfallers.len() == 1` crown gate admits.
///
/// ARM CHOICE (this is the anti-vacuity decision, stated explicitly): the fixture ships AT
/// `WaitingFor::LoopShortcut`, so loading it and asserting the saved payload would drive
/// **0 beats** and prove nothing — the assertion would read the offer the dump was saved
/// with. This row therefore DECLINES the saved offer first, forcing the detector to
/// RE-DERIVE the crown from live beats, and asserts a non-zero driven beat count before
/// asserting anything about the payload. `revive_decline` (reviving P1/P2 to three living
/// opponents) is FORBIDDEN here: at three living opponents the crown gate short-circuits
/// and there is no crown left to assert.
///
/// REVERT-PROBES: ⓐ implement the withdrawn "count + clear_loop_detect_ring + Path-A
/// early-return" remedy ⇒ C's crown disappears as soon as a `TriggerTargetSelection`
/// enters the accumulation — this row is the measured reason that remedy stays withdrawn;
/// ⓑ narrow `find_live_loop_winner` to the first prior frame only ⇒ the crown is lost.
#[test]
fn dump_c_still_crowns_at_one_living_opponent_after_pause_retention() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let mut state = restore_dump(&json);

    assert!(
        state.loop_detection.samples(),
        "reach-guard: sampling mode required; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        engine_live_opponents(&state, P0),
        vec![PlayerId(3)],
        "reach-guard: dump C ships with EXACTLY ONE living opponent (P3) — the only \
         population the CR 732.2a crown gate admits"
    );
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for.clone() else {
        panic!(
            "reach-guard: dump C ships AT a saved offer; got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(proposer, P0);

    // Discard the saved offer so the detector must re-derive it from live beats.
    apply(&mut state, proposer, GameAction::DeclineShortcut).expect("decline the saved offer");

    let pin = engine_live_opponents(&state, P0).first().copied();
    let mut beats = 0usize;
    for _ in 0..200 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        if let Err(why) = dump_drive_one_beat(&mut state, pin) {
            panic!("drive stopped after {beats} beats: {why}");
        }
        beats += 1;
    }

    // ANTI-VACUITY CONTROL: a zero here means the row asserted the dump's saved offer
    // instead of a re-derived one, and both revert-probes above would be inert. The
    // exact count is reported (not just `> 0`) so a change in how far the detector has
    // to drive to re-derive the crown surfaces as a diff rather than passing silently.
    assert_eq!(
        beats, 6,
        "the `c decline` arm re-derives the crown after a measured 6 driven beats — a 0 here \
         would mean the row read the dump's saved offer back instead of re-deriving one, \
         which is what makes both revert-probes above live"
    );

    let WaitingFor::LoopShortcut {
        predicted_winner,
        certificate,
        schema,
        ..
    } = state.waiting_for.clone()
    else {
        panic!(
            "the crown must survive pause retention; after {beats} beats waiting_for was {:?}",
            state.waiting_for
        )
    };
    assert_eq!(predicted_winner, Some(P0), "the crown still names P0");
    assert_eq!(certificate.win_kind, WinKind::LethalDamage);
    assert_eq!(
        certificate.unbounded,
        vec![ResourceAxis::Life(P0), ResourceAxis::Life(PlayerId(3))],
        "the re-derived certificate names the same two life axes as BASE"
    );
    assert!(
        schema.points.is_empty(),
        "a choice-free drain publishes no decision points"
    );
    assert_eq!(schema.iteration_count, IterationCount::UntilLethal);
}

/// Seam D (CR 732.2a): a `template: None` declaration against a NON-EMPTY schema BYPASSES the
/// declare-time pin firewall entirely — `predictability_gate` and `validate_pins` are simply not
/// run, because there is no template to run them against. That bypass is legitimate for exactly
/// one drive shape: the object-growth route, which re-derives its template from
/// `state.last_loop_action_sequence` and never reads `proposal.template`. With an EMPTY sequence
/// there is nothing to re-derive from, so a pin-consuming drive would run with no pins at all.
///
/// This row is the two-conjunct guard's matched pair, on ONE fixture and ONE schema so nothing
/// but the sequence differs between the halves:
///
/// * EMPTY sequence  ⇒ fail-closed manual-play handback (Priority), APNAP never opens.
/// * NON-EMPTY sequence ⇒ APNAP opens unchanged — the reach-guard proving the guard is not a
///   blanket "reject every `template: None`", which would break every shipped object-growth
///   declaration.
///
/// REVERT-PROBE: delete the `None if state.last_loop_action_sequence.is_empty()` arm ⇒ the first
/// half opens `RespondToShortcut` and FAILS. Drop the sequence conjunct instead (reject on
/// `template.is_none()` alone) ⇒ the second half FAILS.
#[test]
fn template_none_against_a_pin_consuming_schema_falls_back_to_manual_play() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};
    use engine::types::identifiers::CardId;

    let source = YieldTarget::ThisObject {
        source_id: ObjectId(1),
        incarnation: None,
        trigger_description: None,
    };
    let schema = ShortcutDecisionSchema {
        iteration_count: IterationCount::UntilLethal,
        max_iterations: ShortcutDecisionSchema::default().max_iterations,
        points: vec![DecisionPoint {
            slot: DecisionSlot { source, index: 0 },
            kind: DecisionPointKind::Targets {
                legal_targets: vec![TargetRef::Player(P1)],
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };

    let declare_with_sequence = |sequence: Vec<LoopActionContext>| -> WaitingFor {
        let (mut runner, _kickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
        runner.state_mut().last_loop_action_sequence = sequence;
        runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner: Some(P0),
            certificate: synthetic_lethal_cert(),
            schema: schema.clone(),
        };
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::UntilLethal,
                template: None,
            })
            .expect("declare dispatch succeeds (a rejection is a manual fallback, not an error)");
        runner.state().waiting_for.clone()
    };

    // The object-growth route's routing signal: a captured recast context. Only its PRESENCE
    // matters to the guard, which is exactly the discriminant `materialize` dispatches on.
    let recast = LoopActionContext {
        card_id: CardId(7),
        controller: P0,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::Used,
        },
        convoke: None,
        pins: Vec::new(),
    };

    let empty_sequence = declare_with_sequence(Vec::new());
    assert!(
        matches!(empty_sequence, WaitingFor::Priority { .. }),
        "CR 732.2a: a pin-consuming schema declared with NO template and NO re-derivable \
         sequence must fail closed to manual play, not open APNAP; got {empty_sequence:?}"
    );

    let with_sequence = declare_with_sequence(vec![recast]);
    assert!(
        matches!(with_sequence, WaitingFor::RespondToShortcut { .. }),
        "reach-guard: the object-growth route re-derives its template from the sequence and \
         must keep opening APNAP — the guard is two-conjunct, not a blanket template-None \
         rejection; got {with_sequence:?}"
    );
}

/// A loop-free board that carries the CR 732.2a ring ACROSS TURN BOUNDARIES and still
/// refuses to offer — the no-false-positive control for widening
/// [`WaitingFor::is_forced_cascade_window`] to the CR 703.1 turn-based actions.
///
/// CR 732.2a says a proposed shortcut "may even cross multiple turns", which is why the
/// class now retains across CR 502.3 untap / CR 508.1 declare attackers / CR 509.1
/// declare blockers / CR 514.1 cleanup discard. Retention is NECESSARY BUT NOT YET
/// SUFFICIENT for that: `loop_states_equal` still compares `turn_number`, so no
/// cross-turn pair certifies today — which is exactly what the ATTRIBUTION half below
/// measures. The risk that widening introduces is the
/// mirror image of the bug it fixes: a ring that now survives turn cycling might
/// accumulate on an ordinary board and certify a loop that isn't there.
///
/// FIXTURE (loop-free by construction, and hostile on purpose): P0 has an upkeep ticker
/// ("At the beginning of your upkeep, you gain 1 life"), a drain cleric (gain ⇒ each
/// opponent loses 1) and a "may draw" scribe (opponent loses life ⇒ optional draw). Each
/// of P0's upkeeps runs a FINITE 3-deep cascade — nothing re-triggers the ticker — yet the
/// per-turn shape is drain-like (P1 loses 1 every other turn), which is exactly the shape
/// a naive detector would mistake for a loop. The cascade ends at the scribe's CR 603.5
/// "may" pause, which leaves the stack already popped, so the sampler's clear arm never
/// fires on the tail resolution and the accumulated frames survive into the rest of the
/// turn. That is what makes cross-turn retention observable on a board with no loop at
/// all.
///
/// NON-VACUITY, both halves, measured (300 beats, 23 turns):
/// * POSITIVE — the ring really is retained across turn boundaries: at beat 58 the drive
///   sits at a `DeclareAttackers` window in turn 6 holding 4 frames whose OLDEST was
///   sampled in turn 4. Without the widening that frame cannot exist: BASE clears at
///   `DeclareAttackers`. So the widening is demonstrably LIVE on this board.
/// * DISCRIMINANT GUARD — `OptionalEffectChoice` is a PRE-EXISTING member of the class and
///   also occurs on this board (10 times). The retention witness is therefore required to
///   be one of the NEWLY exempt turn-based windows; an `OptionalEffectChoice` witness
///   would satisfy the row without the widening being consulted at all.
/// * NEGATIVE — no `LoopShortcut` / `RespondToShortcut` is ever raised, even after the ring
///   saturates at all 16 frames (measured: reached by turn 22, spanning ~9 turn boundaries).
/// * ATTRIBUTION — the decline is a MEASURED comparison failure, not an absent one. The
///   engine's own recurrence gate `loop_states_equal_modulo_resources` reports FALSE on the
///   oldest/newest retained pair, while reporting TRUE on the oldest against itself (the
///   positive control that the comparator is live on this data, trap 7). The monotone axis
///   is named and asserted: each turn's CR 504.1 draw strictly shrinks the library, so no
///   two retained frames can be the same position. Measured at the witness beat: P0's
///   library 60 → 59, P1's 59 → 58. (Honest scope: equalizing library and hand alone does
///   NOT flip the gate to true — turn number and life differ too. The library shrink is
///   asserted as a monotone non-recurrence witness, not as the sole cause.)
///
/// REVERT-PROBE (measured, not predicted): delete the CR 703.1 turn-based members from
/// `is_forced_cascade_window` and the POSITIVE half fails — the retention witness is never
/// found, because `apply_action` clears the ring on the very first `DeclareAttackers` of
/// each turn.
#[test]
fn drawgo_ring_spans_turns_but_never_offers() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(
        P0,
        "Test Upkeep Ticker",
        2,
        2,
        "At the beginning of your upkeep, you gain 1 life.",
    );
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(
        P0,
        "Test May Scribe",
        2,
        2,
        "Whenever an opponent loses life, you may draw a card.",
    );
    // CR 504.1: both players draw every turn, so the libraries must outlast the drive —
    // a deck-out would end the game and silently truncate every assertion below.
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    scenario.with_library_top(P0, &refs);
    scenario.with_library_top(P1, &refs);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let mut state = runner.state().clone();

    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring, making every \
         assertion below vacuous; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the board must start with an EMPTY ring — every frame below is \
         accumulated by this drive"
    );

    // The cross-turn retention witness: (window name, live turn, oldest frame's turn,
    // ring before the answer, ring after it, oldest frame, newest frame).
    let mut witness: Option<(String, u32, u32, usize, usize, GameState, GameState)> = None;
    let mut offer_at: Option<(usize, String)> = None;
    let mut turns_seen: Vec<u32> = Vec::new();

    for beat in 0..300usize {
        if !turns_seen.contains(&state.turn_number) {
            turns_seen.push(state.turn_number);
        }
        let name = state.waiting_for.variant_name().to_string();
        if matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { .. } | WaitingFor::RespondToShortcut { .. }
        ) {
            offer_at = Some((beat, name));
            break;
        }
        // The witness must be a NEWLY exempt CR 703.1 turn-based window, never the
        // pre-existing `OptionalEffectChoice` member (see DISCRIMINANT GUARD above).
        let turn_based = matches!(
            state.waiting_for,
            WaitingFor::UntapChoice { .. }
                | WaitingFor::ChooseUntapSubset { .. }
                | WaitingFor::DeclareAttackers { .. }
                | WaitingFor::ExertChoice { .. }
                | WaitingFor::EnlistChoice { .. }
                | WaitingFor::DeclareBlockers { .. }
                | WaitingFor::DiscardToHandSize { .. }
        );
        let before = state.loop_detect_ring.len();
        let pair = (witness.is_none() && turn_based && before >= 4)
            .then(|| {
                let front = state.loop_detect_ring.front()?.clone();
                let back = state.loop_detect_ring.back()?.clone();
                (front.turn_number < state.turn_number).then_some((front, back))
            })
            .flatten();
        let live_turn = state.turn_number;
        if dump_drive_one_beat(&mut state, None).is_err() {
            break;
        }
        if let Some((front, back)) = pair {
            witness = Some((
                name,
                live_turn,
                front.turn_number,
                before,
                state.loop_detect_ring.len(),
                (*front).clone(),
                (*back).clone(),
            ));
        }
    }

    assert!(
        turns_seen.len() >= 3,
        "reach-guard: the drive must cross at least 2 full turn boundaries for a \
         cross-turn claim to mean anything; saw turns {turns_seen:?}"
    );

    let (window, live_turn, frame_turn, before, after, oldest, newest) = witness.expect(
        "POSITIVE HALF: no CR 703.1 turn-based window (CR 502.3 / CR 508.1 / CR 509.1 / \
             CR 514.1) was ever reached holding >= 4 frames whose oldest was sampled in an \
             EARLIER turn. That is precisely BASE behaviour — dropping the turn-based \
             members from `is_forced_cascade_window` reproduces this failure, because \
             `apply_action` then clears the ring at the first declare-attackers of every \
             turn. Without this witness the no-offer assertion below is vacuous.",
    );
    assert!(
        after >= before,
        "answering the forced turn-based window {window} must not discard the ring \
         (CR 703.1 + CR 117.3a: no player had priority there, so the answer is not a \
         deliberate break); ring went {before} -> {after}"
    );
    assert!(
        frame_turn < live_turn,
        "the retained frame must predate the live turn; frame turn {frame_turn}, live \
         turn {live_turn}"
    );

    assert!(
        offer_at.is_none(),
        "NO-FALSE-POSITIVE: this board has no loop — each upkeep runs a FINITE cascade and \
         nothing re-triggers the ticker — so no CR 732.2a shortcut may ever be offered, \
         however many frames the widened class lets the ring carry across turns. Got an \
         offer at {offer_at:?} (witness: {window} in turn {live_turn} held a turn-{frame_turn} frame)"
    );

    // ATTRIBUTION: the decline is a measured comparison FAILURE on a live comparator,
    // not an absent comparison.
    assert!(
        loop_states_equal_modulo_resources(&oldest, &oldest),
        "positive control (trap 7): the engine's recurrence gate must report TRUE on a \
         retained frame against itself, else the FALSE asserted next is an inert \
         instrument rather than a measured non-recurrence"
    );
    assert!(
        !loop_states_equal_modulo_resources(&oldest, &newest),
        "the turn-{frame_turn} and turn-{} frames must NOT compare recurrent — that \
         comparison failing is WHY no offer forms",
        newest.turn_number
    );
    for (i, (old_p, new_p)) in oldest.players.iter().zip(newest.players.iter()).enumerate() {
        assert!(
            new_p.library.len() < old_p.library.len(),
            "CR 504.1: every turn's draw strictly shrinks each library, which is the \
             monotone axis that makes two retained frames un-recurrable. P{i} went \
             {} -> {} across the retained window",
            old_p.library.len(),
            new_p.library.len()
        );
    }
}

// ===========================================================================
// CR 510.2 EVENT-KEYED loop-ring invalidation.
//
// `WaitingFor::AssignCombatDamage` / `AssignBlockerDamage` are excluded from
// `is_forced_cascade_window` because CR 510.2 deals the assigned damage with no
// intervening priority. That WINDOW-keyed exclusion is necessary but NOT
// sufficient: the window opens only when a damage DIVISION choice is required
// (`game::combat_damage`: "Auto-assign for unblocked, single blocker, or
// blocked-but-no-current-blockers"). An UNBLOCKED attacker moves a life total
// with NO window to exclude — and with the CR 703.1 turn-based members now in
// the class, `DeclareAttackers` / `DeclareBlockers` no longer clear the ring
// either, so the ring rides straight through the life change.
//
// The sufficient guard is `GameState::invalidate_loop_ring_on_unobserved_life_move`,
// called at the end of `apply_combat_damage` — the CR 510.2 batch itself.
// ===========================================================================

fn player_life(state: &GameState, p: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.life)
        .expect("seat exists")
}

/// `dump_drive_one_beat`, but it actually fights.
///
/// MEASURED, and the reason this helper exists: at a CR 508.1 / CR 509.1 declaration the
/// generic driver takes the FIRST legal action, and that is the EMPTY declaration —
/// 14 `DeclareAttackers` windows over 400 beats produced
/// `DeclareAttackers { attacks: [], bands: [] }` every time and ZERO combat damage. A row
/// about CR 510.2 driven by that policy is vacuous by construction. Here the largest
/// non-empty declaration wins, so the attack and the block both really happen; every
/// other window keeps the shared policy.
fn combat_drive_one_beat(state: &mut GameState) -> Result<Vec<GameEvent>, String> {
    if matches!(
        state.waiting_for,
        WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. }
    ) {
        if let Some((who, actions)) = dump_beat_actor(state) {
            let biggest = actions
                .iter()
                .filter_map(|a| match a {
                    GameAction::DeclareAttackers { attacks, .. } => Some((attacks.len(), a)),
                    GameAction::DeclareBlockers { assignments } => Some((assignments.len(), a)),
                    _ => None,
                })
                .max_by_key(|(n, _)| *n)
                .filter(|(n, _)| *n > 0)
                .map(|(_, a)| a.clone());
            if let Some(action) = biggest {
                return apply(state, who, action.clone())
                    .map(|r| r.events)
                    .map_err(|e| format!("apply err ({action:?}): {e:?}"));
            }
        }
    }
    dump_drive_one_beat(state, None)
}

/// The shared board for both CR 510.2 rows. P0 runs the same loop-FREE upkeep cascade
/// `drawgo_ring_spans_turns_but_never_offers` uses (ticker → drain cleric → "may" scribe),
/// which is what accumulates a CR 732.2a ring at all; the trio carries Defender so the
/// only creature that can attack is the dedicated 3/3, making the combat shape of each
/// row a deliberate fixture property rather than an artifact of which creature the driver
/// happened to declare.
///
/// `p1_wall` gives P1 a single 0/20 blocker. With it, the attack is BLOCKED and CR 510.2
/// moves no player's life (creature-only damage). Without it, the attacker is UNBLOCKED
/// and CR 510.2 moves P1's life with no assignment window — CR 510.1c's window needs 2+
/// blockers to divide damage among.
fn combat_ring_board(p1_wall: bool) -> GameState {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // Deep life totals on BOTH seats: the drive must outlast several turn cycles of
    // combat damage plus the cleric's drain, and a CR 704.5a death would end the game
    // and silently truncate every assertion below.
    scenario.with_life(P0, 400);
    scenario.with_life(P1, 400);
    scenario.add_creature_from_oracle(
        P0,
        "Test Upkeep Ticker",
        2,
        2,
        "Defender\nAt the beginning of your upkeep, you gain 1 life.",
    );
    scenario.add_creature_from_oracle(
        P0,
        "Test Drain Cleric",
        2,
        2,
        &format!("Defender\n{DRAIN_CLERIC}"),
    );
    scenario.add_creature_from_oracle(
        P0,
        "Test May Scribe",
        2,
        2,
        "Defender\nWhenever an opponent loses life, you may draw a card.",
    );
    scenario.add_creature(P0, "Test Lone Attacker", 3, 3);
    if p1_wall {
        // 0 power so the trade kills nothing and the block repeats every turn cycle;
        // toughness 20 so the 3/3 never kills it either. Defender is load-bearing, not
        // flavour: without it the wall attacks on P1's turn, is still TAPPED on P0's, and
        // cannot block — measured, the attack then went through unblocked and the row
        // silently became a duplicate of the unblocked one.
        scenario.add_creature_from_oracle(P1, "Test Wall", 0, 20, "Defender");
    }
    // CR 504.1: both players draw every turn, so the libraries must outlast the drive —
    // a deck-out would end the game and truncate the row.
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    scenario.with_library_top(P0, &refs);
    scenario.with_library_top(P1, &refs);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state().clone()
}

/// HIGH-1: the CR 510.2 damage EVENT clears the CR 732.2a ring even though no
/// `AssignCombatDamage` window ever opens.
///
/// FIXTURE: P0 attacks with one unblocked 3/3 into an empty board while the loop-free
/// upkeep cascade keeps the ring populated. CR 510.1c's assignment window needs a
/// division choice, so on this board it never opens at all — which is exactly the hole
/// the window-keyed exclusion leaves and the reason the fence has to be event-keyed.
///
/// ASSERTIONS, in the order they discharge each other:
/// 1. REACH-GUARD — the drive reaches a CR 510.2 beat that damaged P1 while the ring
///    already carried >= 2 frames. Without that, "the ring is empty afterwards" is
///    unobservable: an already-empty ring would satisfy it.
/// 2. DISCRIMINANT GUARD — no `AssignCombatDamage` / `AssignBlockerDamage` window is
///    observed anywhere in the drive. This row's whole point is that the damage lands
///    with NO window; a fixture drift that introduces a division choice would make the
///    window-keyed exclusion sufficient and the row vacuous, so it fails loudly instead.
/// 3. LIFE-MOVE WITNESS — P1's life strictly decreased across that beat, so assertion 4
///    is about a real CR 119.3 / CR 120.3a life movement.
/// 4. THE DELIVERABLE — the ring is empty immediately after the beat.
///
/// REVERT-PROBE (measured, recorded in the handoff report): delete the
/// `invalidate_loop_ring_on_unobserved_life_move` call from `apply_combat_damage` ⇒
/// assertion 4 FAILS while 1–3 still PASS, which is what proves 4 is the discriminator.
#[test]
fn unblocked_attacker_damage_clears_the_loop_ring_with_no_window() {
    let mut state = combat_ring_board(false);

    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: every frame below is accumulated by this drive, not preloaded"
    );

    let mut assignment_window: Option<String> = None;
    // (beat, ring before, ring after, P1 life before, P1 life after, combat damage)
    let mut witness: Option<(usize, usize, usize, i32, i32, u32)> = None;
    let mut max_ring = 0usize;
    let mut damage_beats = 0usize;

    for beat in 0..400usize {
        if matches!(
            state.waiting_for,
            WaitingFor::AssignCombatDamage { .. } | WaitingFor::AssignBlockerDamage { .. }
        ) {
            assignment_window.get_or_insert_with(|| state.waiting_for.variant_name().to_string());
        }
        let before = state.loop_detect_ring.len();
        max_ring = max_ring.max(before);
        let life_before = player_life(&state, P1);
        let Ok(events) = combat_drive_one_beat(&mut state) else {
            break;
        };
        let dealt: u32 = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CombatDamageDealtToPlayer {
                    player_id,
                    total_damage,
                    ..
                } if *player_id == P1 => Some(*total_damage),
                _ => None,
            })
            .sum();
        if dealt > 0 {
            damage_beats += 1;
            if witness.is_none() && before >= 2 {
                witness = Some((
                    beat,
                    before,
                    state.loop_detect_ring.len(),
                    life_before,
                    player_life(&state, P1),
                    dealt,
                ));
                // The evidence is complete; the rest of the drive only costs time. The
                // window guard has already covered every beat up to here, and the loop
                // below re-checks the settled window once more.
                break;
            }
        }
    }
    if matches!(
        state.waiting_for,
        WaitingFor::AssignCombatDamage { .. } | WaitingFor::AssignBlockerDamage { .. }
    ) {
        assignment_window.get_or_insert_with(|| state.waiting_for.variant_name().to_string());
    }

    let (beat, before, after, life_before, life_after, dealt) = witness.unwrap_or_else(|| {
        panic!(
            "reach-guard: no CR 510.2 beat dealt combat damage to P1 while the ring held \
             >= 2 frames, so the clear below would be unobservable. Combat-damage beats \
             seen: {damage_beats}; max ring: {max_ring}. ATTRIBUTION: if \
             `is_forced_cascade_window` no longer exempts \
             `DeclareAttackers`/`DeclareBlockers`, the ring is wiped before combat and \
             this guard reds first — read that as a class-membership regression, not as a \
             failure of the combat-damage fence below"
        )
    });

    assert!(
        assignment_window.is_none(),
        "DISCRIMINANT GUARD: this row exists because an UNBLOCKED attacker deals CR 510.2 \
         damage with NO window — CR 510.1c's assignment window opens only for a division \
         choice. A {assignment_window:?} window means the fixture drifted into the case \
         the window-keyed exclusion already covers, making the row vacuous."
    );
    assert!(
        life_after < life_before,
        "LIFE-MOVE WITNESS: CR 120.3a — {dealt} combat damage to P1 must have reduced its \
         life; got {life_before} -> {life_after} at beat {beat}"
    );
    assert!(
        before >= 2,
        "reach-guard: the ring must carry >= 2 frames INTO the damage beat; got {before}"
    );
    assert_eq!(
        after, 0,
        "CR 510.2 + CR 704.5a: the damage batch moved a life total with no intervening \
         priority, so the ring accumulated before it may not be compared across it — it \
         must be EMPTY after the beat. Ring went {before} -> {after} at beat {beat} \
         (P1 {life_before} -> {life_after}). Deleting the \
         `invalidate_loop_ring_on_unobserved_life_move` call from `apply_combat_damage` \
         reproduces this failure: with `DeclareAttackers` / `DeclareBlockers` in the \
         forced-cascade class and no assignment window ever opening, nothing else clears \
         here."
    );
}

/// The matched negative: CR 510.2 damage that moves NO player's life leaves the ring
/// alone. Same board, but P1 fields a single 0/20 wall, so the 3/3 is blocked and the
/// whole batch is creature-to-creature.
///
/// This pins the `p.life != before` predicate as load-bearing. Replacing it with an
/// unconditional `clear()` — "clear on every combat damage" — still passes the row above
/// but flips this one to FAIL, and would be a needless retention regression on every
/// board where creatures merely trade.
///
/// NON-VACUITY: the witness requires a beat that BOTH dealt combat damage to a creature
/// (`DamageDealt { target: Object, is_combat: true }`) AND left every player's life
/// unchanged, with the ring already carrying >= 2 frames. A beat where no combat happened
/// cannot satisfy it, so the row cannot pass by the attack never occurring. The single
/// blocker also keeps CR 510.1c's division window shut, matching the row above.
#[test]
fn creature_only_combat_damage_leaves_the_loop_ring_intact() {
    let mut state = combat_ring_board(true);

    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring; got {:?}",
        state.loop_detection
    );

    // (beat, ring before, ring after, creature damage dealt)
    let mut witness: Option<(usize, usize, usize, u32)> = None;
    let mut max_ring = 0usize;
    let mut creature_damage_beats = 0usize;

    for beat in 0..400usize {
        let before = state.loop_detect_ring.len();
        max_ring = max_ring.max(before);
        let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        let Ok(events) = combat_drive_one_beat(&mut state) else {
            break;
        };
        let to_creatures: u32 = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt {
                    target: TargetRef::Object(_),
                    amount,
                    is_combat: true,
                    ..
                } => Some(*amount),
                _ => None,
            })
            .sum();
        let lives_after: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        if to_creatures > 0 && lives_after == lives_before {
            creature_damage_beats += 1;
            if witness.is_none() && before >= 2 {
                witness = Some((beat, before, state.loop_detect_ring.len(), to_creatures));
                break;
            }
        }
    }

    let (beat, before, after, dealt) = witness.unwrap_or_else(|| {
        panic!(
            "reach-guard: no beat dealt CR 510.2 damage to a creature with every player's \
             life unchanged while the ring held >= 2 frames. Creature-damage beats seen: \
             {creature_damage_beats}; max ring: {max_ring}. ATTRIBUTION: if \
             `is_forced_cascade_window` no longer exempts \
             `DeclareAttackers`/`DeclareBlockers`, the ring is wiped before combat and \
             this guard reds first — read that as a class-membership regression, not as a \
             failure of the combat-damage fence below"
        )
    });

    assert!(
        after >= before,
        "CR 119.3: no player's life moved in this CR 510.2 batch ({dealt} damage, all of it \
         to creatures), so there is nothing for the loop-ring prohibition to fence and the \
         accumulated ring must SURVIVE. Ring went {before} -> {after} at beat {beat}. \
         Replacing the `p.life != before` predicate in \
         `invalidate_loop_ring_on_unobserved_life_move` with an unconditional `clear()` \
         reproduces this failure."
    );
}

// ===========================================================================
// X1-1 — CR 117.1b on a REAL 4-player dump.
// ===========================================================================

/// Sprout Swarm in P0's hand in the dump-A capture.
const X1_SPROUT: ObjectId = ObjectId(64);
/// An untapped P0 fodder Saproling to convoke for the {G}.
const X1_FODDER: ObjectId = ObjectId(421);

/// X1-1 (⛔ the §H.2-gated row). The real 4-player Witherbloom / Sprout Swarm /
/// Lumaret capture: P0 drives a Saproling object-growth loop while three opponents sit
/// on utility lands whose activated abilities read the growing class, plus P0's own
/// Jadar (a `{Phase, End}` observer). Pre-fix the CR 732.2a firewall vetoed and no offer
/// surfaced.
///
/// ⛔ BLOCKING PRECONDITIONS (plan §H.2), MEASURED BEFORE THIS ROW WAS WRITTEN, at the
/// C-2 firewall call on this exact board:
/// * `scope.sole_driver == Some(PlayerId(0))` — the driving player. X1's own key.
/// * `scope.phase_invariant == Some(PreCombatMain)` — the value is REPORTED here, not
///   pre-asserted: asserting a literal on a loaded dump would smuggle in an unverified
///   premise. The row asserts only that the guard was reachable.
/// * `trigger_event_unreachable_in_phase(<Jadar obj 75: mode=Phase, phase=Some(End),
///   damage_kind=Any>, PreCombatMain) == true` — SUFFICIENCY, not just reachability:
///   the dump's veto set spans BOTH classes (4 of 5 blockers are X1-class opponent
///   lands, the 5th is Jadar in the X2 class), so the offer needs both guards to fire.
///   Instrument control on the same run: 48 `true` / 208 `false` over the board's
///   trigger population, so the predicate is not constant.
///
/// ⛔ HONEST EVIDENCE BASIS: BASE is a measured no-offer trajectory whose FIRST veto was
/// object 75. First-veto evidence bounds NOTHING about the remaining veto set — the
/// firewall returns on the first `true` (13 `return true` sites in
/// `fire_time_conditions_read_growing_class_scoped`). The offer-level assertion below is
/// what carries this row's claim; the BASE figure is provenance, not proof.
///
/// REVERT-PROBE: delete the `obj.controller != driver` conjunct in block (2) ⇒ the
/// opponents' utility-land abilities veto again ⇒ the offer disappears ⇒ FAILS.
#[test]
fn witherbloom_lumaret_4p_offers_with_opponent_utility_lands() {
    use engine::types::ability::AbilityKind;
    use engine::types::game_state::LoopDetectionMode;
    use engine::types::zones::Zone;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/witherbloom_sprout_lumaret_4p.json.gz"
    )));
    state.loop_detection = LoopDetectionMode::On;

    // ── fixture preconditions (hold in BOTH revert modes ⇒ the offer is non-vacuous) ──
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "fixture precondition: ordinary P0 priority pre-cast, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state
            .objects
            .get(&X1_SPROUT)
            .map(|o| (o.name.as_str(), o.zone)),
        Some(("Sprout Swarm", Zone::Hand)),
        "fixture precondition: Sprout Swarm is in P0's hand"
    );
    let fodder = state.objects.get(&X1_FODDER).expect("fodder present");
    assert!(
        fodder.name == "Saproling" && fodder.controller == P0 && !fodder.tapped,
        "fixture precondition: an untapped P0 Saproling to convoke"
    );
    // The X1 class is really present: opponents control battlefield permanents.
    let foreign_permanents = state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.controller != P0))
        .count();
    assert!(
        foreign_permanents >= 3,
        "fixture precondition: the dump carries opponent-controlled permanents (the X1 \
         class); got {foreign_permanents}"
    );

    // ── SHAPE, not just a count. This offer rests on the X1 (`obj.controller != driver`)
    // relief, and item A narrows that relief to `kind == AbilityKind::Activated` with
    // `activator_filter.is_none()`. A bare `foreign_permanents >= 3` count cannot tell
    // whether the relieved population is the one item A governs; this does.
    let foreign_ability_kinds: Vec<AbilityKind> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller != P0)
        .flat_map(|o| o.abilities.iter().map(|a| a.kind))
        .collect();
    assert!(
        !foreign_ability_kinds.is_empty(),
        "fixture precondition: the foreign battlefield ability population must be NON-EMPTY, \
         else item A's `kind == Activated` narrowing has nothing to act on here and this \
         row's offer is not evidence about X1 at all"
    );
    assert!(
        foreign_ability_kinds
            .iter()
            .all(|k| *k == AbilityKind::Activated),
        "fixture precondition: every foreign battlefield ability def must be `Activated` — \
         item A relieves ONLY that kind, so a non-`Activated` def here would keep vetoing \
         and the offer would be attributable to something else; got {foreign_ability_kinds:?}"
    );
    assert!(
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.controller != P0)
            .all(|o| o.abilities.iter().all(|a| a.activator_filter.is_none())),
        "fixture precondition: no foreign def carries an `activator_filter` — item E refuses \
         relief on ANY `Some(..)`, so one here would suppress this offer"
    );
    let jadar = state
        .objects
        .get(&engine::types::identifiers::ObjectId(75))
        .expect("fixture precondition: object 75 is present");
    assert_eq!(
        (jadar.name.as_str(), jadar.zone, jadar.controller),
        ("Jadar, Ghoulcaller of Nephalia", Zone::Battlefield, P0),
        "fixture precondition: the driver-side observer this row names"
    );
    assert_eq!(
        jadar.trigger_definitions.len(),
        1,
        "fixture precondition: Jadar carries exactly one trigger definition; got {}",
        jadar.trigger_definitions.len()
    );

    let outcome = GameRunner::from_state(state)
        .cast(X1_SPROUT)
        .accept_optional()
        .convoke_with(&[X1_FODDER])
        .commit()
        .resolve();

    // ── reach-guard: the cast really resolved and grew the class ──
    assert_eq!(
        outcome.zone_of(X1_SPROUT),
        Zone::Hand,
        "reach-guard: Buyback returned Sprout Swarm to P0's hand"
    );

    // ── DISCRIMINATOR: the CR 732.2a offer surfaces ──
    match outcome.final_waiting_for() {
        WaitingFor::LoopShortcut {
            proposer,
            predicted_winner,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0, "the driver proposes");
            assert_eq!(
                *predicted_winner, None,
                "an Advantage offer has no predicted winner"
            );
            assert_eq!(
                certificate.win_kind,
                WinKind::Advantage,
                "CR 732.2a: this is a beneficial (advantage) loop, not a mandatory win"
            );
            assert!(
                certificate.unbounded.contains(&ResourceAxis::TokensCreated),
                "the unbounded axes must include TokensCreated, got {:?}",
                certificate.unbounded
            );
        }
        other => panic!(
            "X1-1: CR 117.1b — no player but the sole driver receives priority inside the \
             taken shortcut, so the opponents' utility-land abilities cannot read the \
             growing class and must not suppress the offer; got {other:?}. \
             ⛔ PRE-REGISTERED STOP BRANCH: do NOT widen X1's conjunct, X2's arms, or any \
             downstream gate to manufacture this offer. Run the veto-enumeration \
             diagnostic (convert the 13 `return true` sites in \
             `fire_time_conditions_read_growing_class_scoped` to log-and-continue, replay, \
             record every vetoing object id and its block), name the next rejecter and its \
             call count in the PR body, and STOP."
        ),
    }
}

// ===========================================================================
// K4 — CR 608.2i + CR 608.2j ledger-FILTER exclusion (the shallow BB-FU10-N narrowing).
// Every fixture carries the harness's shared `"Flying, trample\n"` keyword prefix, so
// subject and control differ ONLY in the ledger clause.
// ===========================================================================

/// FIXTURE C (PRIMARY) — measured `mode=DamageDone`, `phase=null`, `damage_kind=Any`,
/// `constraint=null`. `damage_kind: Any` is what makes this pair STRUCTURALLY independent
/// of the CR 510.2 phase relief, whose damage arm requires `CombatOnly`.
const LEDGER_ARTIFACT_FILTER_ORACLE: &str = "Flying, trample\nWhenever this creature deals damage to a player, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";

/// FIXTURE D (PRIMARY) — fixture C with one Oracle noun changed. Measured: the two
/// serialized trigger definitions are 984 bytes each and differ at exactly TWO token
/// positions — `filter.type_filters[0]` and the humanized `description` string — both
/// projections of that ONE noun, and `description` is a display string no scan predicate
/// reads.
const LEDGER_CREATURE_FILTER_ORACLE: &str = "Flying, trample\nWhenever this creature deals damage to a player, draw a card if you had two or more creatures enter the battlefield under your control this turn.";

/// FIXTURE A (CORROBORATING) — a DIFFERENT `TriggerMode`. Measured `mode=Phase`,
/// `phase=PreCombatMain`, `damage_kind=Any`, `constraint=OnlyDuringYourTurn`. Its
/// independence from the phase relief rests on the ⛔ STRICT-INEQUALITY pin
/// (`p != phase`, so `PreCombatMain` in a `PreCombatMain` window is NOT relieved) — hence
/// corroborating rather than primary.
const PHASE_LEDGER_ARTIFACT_FILTER_ORACLE: &str = "Flying, trample\nAt the beginning of your precombat main phase, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";

/// FIXTURE B (CORROBORATING) — fixture A one Oracle noun apart.
const PHASE_LEDGER_CREATURE_FILTER_ORACLE: &str = "Flying, trample\nAt the beginning of your precombat main phase, draw a card if you had two or more creatures enter the battlefield under your control this turn.";

/// K4-N1 (PRIMARY) — CR 608.2i + CR 608.2j. A ledger observer whose entry filter PROVABLY cannot
/// count the growing fodder has a read whose value is invariant across the loop's growth,
/// so it does not observe the loop and must not suppress the CR 732.2a offer.
///
/// ATTRIBUTION, structural rather than argued:
/// * the CR 510.2 relief cannot move this row — `damage_kind: Any` (measured) can never
///   satisfy its damage arm, which requires `CombatOnly` (pinned by
///   `trigger_event_unreachable_in_phase_shape_is_pinned` arm 2), and `mode: DamageDone`
///   never reaches its Phase arm.
/// * the CR 117.1b relief cannot move it — the bystander is the DRIVER'S OWN.
///   ⇒ the flip is attributable to the ledger-filter narrowing alone.
///
/// REVERT-PROBES: (1) delete the `&& !class_members.is_some_and(..)` guard ⇒ veto ⇒ FAILS.
/// (2) make `execute_ledger_condition_provably_excludes_class` return `false`
/// unconditionally ⇒ the same failure ⇒ the PREDICATE, not the plumbing, carries the flip.
#[test]
fn noncombat_damage_ledger_observer_whose_filter_excludes_the_class_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    // (2) ANTI-VACUITY CONTROL, granted in BOTH builds.
    let (control_runner, _) = object_growth_with_bystander(PLAIN_DRAW_TRIGGER_ORACLE);
    assert!(
        matches!(
            control_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "(2) control: a plain draw-trigger bystander must not suppress the offer"
    );

    let (runner, bystander) = object_growth_with_bystander(LEDGER_ARTIFACT_FILTER_ORACLE);

    // (3) reach-guards.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "(3) reach-guard: exactly one trigger definition carries the ledger read"
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "(1) unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "(1) CR 608.2j: a `Typed{{Artifact}}` entry filter cannot count a Saproling \
             creature token, so the observer's read is invariant across the loop's growth \
             and must not suppress the offer; got {other:?}. \
             ⛔ PRE-REGISTERED FAILURE BRANCH: report the NEXT rejecter by name and its \
             call count and STOP — do not widen a conjunct to manufacture the offer. \
             Conjunct (a) is measured to pass; the remaining candidates in order are (c) \
             and the offer-path gates downstream of the firewall."
        ),
    }
}

/// K4-N2 (PRIMARY) — THE ROW THAT KILLS THE LAZY-BUT-UNSOUND NARROWING. Fixture D is
/// fixture C with one Oracle noun changed, and its `Typed{Creature}` filter GENUINELY
/// counts the Saproling creature token the loop creates each cycle. So the veto must
/// survive.
///
/// This pair IS the acceptance criterion: a correct narrowing moves K4-N1 and not this
/// row; a blanket relaxation moves both; an inert guard moves neither.
///
/// REVERT-PROBE: make conjunct (c) unconditionally `true` (a blanket relaxation) ⇒ this
/// row flips to an offer ⇒ FAILS.
#[test]
fn noncombat_damage_ledger_observer_whose_filter_matches_the_class_still_suppresses_offer() {
    use engine::types::zones::Zone;

    let (runner, bystander) = object_growth_with_bystander(LEDGER_CREATURE_FILTER_ORACLE);

    // (3) reach-guards. Anti-vacuity for a VETO row: the sibling POSITIVE
    // `noncombat_damage_ledger_observer_whose_filter_excludes_the_class_does_not_suppress_offer`
    // shows the same board DOES offer when the filter excludes, so this row's veto is
    // attributable to the filter and not to the board.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (1) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: exactly one trigger definition carries the ledger read; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "CR 608.2j: a `Typed{{Creature}}` entry filter DOES count a Saproling creature \
         token, so the observer genuinely observes the loop and must keep vetoing; got {:?}",
        runner.state().waiting_for
    );
}

/// K4-N4a (CORROBORATING) — the same relief through a DIFFERENT `TriggerMode`, which is
/// what proves it keys on the ledger FILTER and not on any one trigger shape.
///
/// ⚠ Independence from the CR 510.2 relief is CONDITIONAL on the ⛔ strict-inequality pin
/// (`p != phase`): fixture A is `phase: Some(PreCombatMain)` in a `PreCombatMain` window,
/// so the phase arm answers `false` and cannot classify it. Hence corroborating.
#[test]
fn phase_reachable_ledger_observer_whose_filter_excludes_the_class_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let (runner, bystander) = object_growth_with_bystander(PHASE_LEDGER_ARTIFACT_FILTER_ORACLE);

    // (3) reach-guards, ALL BEFORE the offer match. This is a POSITIVE row: a parse failure
    // yields no observer at all and the offer would form trivially, so these guards are what
    // make that vacuity mode loud.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (1) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: the ledger observer must have PARSED — a misparse leaves zero trigger \
         defs and the offer below forms for the wrong reason; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "K4-N4a: unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "K4-N4a CR 608.2j: a phase-REACHABLE observer whose entry filter excludes the \
             fodder must not suppress the offer; got {other:?}"
        ),
    }
}

/// K4-N4b (CORROBORATING) — fixture B, one Oracle noun from K4-N4a, keeps its veto.
///
/// REVERT-PROBE: make conjunct (c) unconditional ⇒ flips ⇒ FAILS.
#[test]
fn phase_reachable_ledger_observer_whose_filter_matches_the_class_still_suppresses_offer() {
    use engine::types::zones::Zone;

    let (runner, bystander) = object_growth_with_bystander(PHASE_LEDGER_CREATURE_FILTER_ORACLE);

    // (3) reach-guards. Anti-vacuity for a VETO row: the sibling POSITIVE
    // `phase_reachable_ledger_observer_whose_filter_excludes_the_class_does_not_suppress_offer`
    // shows the same board DOES offer when the filter excludes.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: block (1) hard-skips non-battlefield zones"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: exactly one trigger definition carries the ledger read; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "K4-N4b: the matching half of the corroborating pair must keep vetoing; got {:?}",
        runner.state().waiting_for
    );
}

// ===========================================================================
// R6a — the ∞ badge is a lie once the collapse is SCHEDULED, and CR 732.2c
// bounds the boundary prompt by the count the table accepted.
// ===========================================================================

/// Sprout Swarm in P0's hand in the `witherbloom_sprout_lumaret_simple_4p` capture.
const R6A_SPROUT: ObjectId = ObjectId(405);
/// The one untapped P0 Saproling in that capture — the {G} convoke fodder.
const R6A_FODDER: ObjectId = ObjectId(1412);

/// Load the simple 4p Witherbloom/Sprout capture and drive one real buyback+convoke
/// recast through the cast pipeline, returning the state AT the CR 732.2a offer.
fn r6a_offer_state() -> GameState {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/witherbloom_sprout_lumaret_simple_4p.json.gz"
    )));
    state.loop_detection = LoopDetectionMode::On;
    let outcome = GameRunner::from_state(state)
        .cast(R6A_SPROUT)
        .accept_optional()
        .convoke_with(&[R6A_FODDER])
        .commit()
        .resolve();
    outcome.state().clone()
}

/// Proposer declares `Fixed(n)`; every living opponent accepts (APNAP).
fn r6a_declare_and_accept_all(state: &mut GameState, proposer: PlayerId, n: u32) {
    apply(
        state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        },
    )
    .expect("the proposer declares the object-growth shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

/// Pass priority through the real production path until the CR 500.5 step/phase
/// boundary surfaces a non-`Priority` prompt (the `LoopCollapse` pay-amount) or the
/// phase advances with no prompt. Bounded so a wedge fails loudly.
fn r6a_drive_to_boundary(state: &mut GameState) {
    let start_phase = state.phase;
    for _ in 0..64 {
        let WaitingFor::Priority { player } = state.waiting_for.clone() else {
            return;
        };
        apply(state, player, GameAction::PassPriority)
            .expect("pass priority toward the next phase boundary");
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.phase != start_phase {
            return;
        }
    }
    panic!("r6a_drive_to_boundary: no phase boundary within 64 passes");
}

/// R6a-1 (PRIMARY). MEASURED DEFECT: accepting the Witherbloom/Sprout loop writes
/// `unbounded_resources = {P0: [Life(0), TokensCreated]}` and that mark survives to the
/// CR 500.5 boundary, so the HUD renders an "∞ Life" badge beside P0's *finite*, growing
/// life total. CR 732.2c: once the last player accepted, the shortcut IS taken at the
/// named `Fixed(N)` — the growth is bounded, so no `∞` row may render for a scheduled axis.
///
/// FILTER THE PROJECTION, NEVER THE STORE. The store must still carry the mark (it is what
/// CR 104.4b / CR 110.1 lockstep and `zones::apply_zone_exit_cleanup`'s defuse read until
/// the boundary applies the growth), so this row asserts BOTH halves.
///
/// §15 NON-VACUITY: the emptiness assertion is paired with the store's NON-emptiness and a
/// non-empty `unbounded_loop_pile` — the same `state` the projection reads is measurably
/// populated, so the instrument demonstrably CAN report a row. Emptiness is asserted on the
/// WIRE (`derive_views`), never on `state`.
///
/// REVERT-PROBES (both RUN, both observed to fail):
/// ⓐ delete the `if scheduled.contains(&axis) { continue; }` filter in `derive_views`
///    ⇒ the `Life(0)` and `TokensCreated` rows render ⇒ assertion (3) FAILS.
/// ⓑ make `GameState::scheduled_collapse_axes` return an empty set ⇒ (3) FAILS the same
///    way AND `clear_collapsed_materializations` stops removing at the boundary — which is
///    what proves the projection and the collapse share ONE authority rather than two
///    copies of the same match.
#[test]
fn scheduled_collapse_renders_no_unbounded_badge() {
    let mut state = r6a_offer_state();

    // (0) reach-guard: the real cast reached the CR 732.2a offer.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the buyback+convoke recast must surface P0's offer, got {:?}",
        state.waiting_for
    );

    r6a_declare_and_accept_all(&mut state, P0, 200);

    // (1) POSITIVE CONTROL — the accept really marked the ∞ axes in the STORE. Without
    // this the emptiness in (3) would be vacuous.
    let marked = state
        .unbounded_resources
        .get(&P0)
        .expect("accept must mark P0's ∞ axes in the store")
        .clone();
    assert!(
        marked.contains(&ResourceAxis::Life(P0)),
        "MEASURED defect axis: the accept marks Life(P0) ∞, got {marked:?}"
    );
    assert!(
        marked.contains(&ResourceAxis::TokensCreated),
        "the accept marks TokensCreated ∞, got {marked:?}"
    );
    assert!(
        !state.unbounded_loop_pile.is_empty(),
        "the object-growth accept writes a non-empty ∞ pile (store still populated)"
    );
    assert_eq!(
        state.pending_unbounded_materialization.len(),
        1,
        "exactly one controller has a scheduled collapse"
    );
    // The growth really is finite: P0's life is a concrete number, not ∞.
    let life = state.players.iter().find(|p| p.id == P0).unwrap().life;
    assert!(
        life > 0,
        "the axis the badge lies about is a finite life total, got {life}"
    );

    // (2) FAIL-CLOSED CONTROL, in the SAME state: every ∞ axis the accept scheduled is
    // covered by the shared authority. An axis it does not name keeps its badge (R6a-2/-3).
    let scheduled = state.scheduled_collapse_axes(
        state
            .pending_unbounded_materialization
            .get(&P0)
            .expect("stash present"),
    );
    assert!(
        marked.iter().all(|a| scheduled.contains(a)),
        "every marked axis on this board is scheduled; marked={marked:?} scheduled={scheduled:?}"
    );

    // (3) DISCRIMINATOR — on the WIRE, for EVERY viewer (and the spectator view), no ∞ row.
    // ALL THREE ∞ surfaces share the one authority, so the HUD can never hide a resource badge
    // while a card group still renders ∞. The PER-SURFACE positive rows live on their own real
    // fixtures — `combo_infinite_pile::real_4p_object_growth_accept_writes_infinite_pile` (pile)
    // and `kilo_live_offer_from_real_dump::kilo_accept_marks_pentad_charge_as_unbounded_display_
    // target` (counter pills) — so a regression on ONE surface stays visible even though this
    // row flips on all of them at once.
    for viewer in [None, Some(P0), Some(P1), Some(P2), Some(PlayerId(3))] {
        let views = engine::game::derived_views::derive_views(&state, viewer);
        assert!(
            views.unbounded_resources.is_empty(),
            "CR 732.2c: a scheduled finite collapse must render NO ∞ row (viewer {viewer:?}), \
             got {:?}",
            views.unbounded_resources
        );
        assert!(
            views.unbounded_pile.is_empty(),
            "CR 732.2c: ...and no ∞ card group beside it (viewer {viewer:?}), got {:?}",
            views.unbounded_pile
        );
    }

    // (3b) A TRIPWIRE, NOT A SECOND PRODUCER. Multiplayer broadcasts (`phase-server`) and the
    // WASM `wrap_filtered` getter go through `derive_filtered_views`, which CALLS
    // `derive_views(filtered_state, viewer)` and then overrides only
    // `unique_authorized_submitter` and `blocker_assignment_pairs`. It WRAPS; it does not
    // bypass. So gating in `derive_views` alone could not have leaked ∞ to the broadcast
    // path — there is no other producer of these three fields, and this row costs zero
    // production code.
    //
    // What it DOES guard is the INPUT: `filter_state_for_viewer` is a clone-and-redact with
    // ZERO `unbounded` references today, so it passes `pending_unbounded_materialization`
    // through unredacted and the gate sees the same stash the hot-seat viewer does. If a
    // future redaction ever drops that stash from the filtered clone, the gate goes silently
    // INERT on the broadcast path only — filtered viewers get the ∞ rows back while the local
    // viewer does not. That is the regression this row catches.
    for viewer in [P0, P1, P2, PlayerId(3)] {
        let filtered = engine::game::visibility::filter_state_for_viewer(&state, viewer);
        let views =
            engine::game::derived_views::derive_filtered_views(&state, &filtered, Some(viewer));
        assert!(
            views.unbounded_resources.is_empty() && views.unbounded_pile.is_empty(),
            "CR 732.2c: the viewer-FILTERED broadcast path hides the same rows (viewer \
             {viewer:?}), got {:?} / {:?}",
            views.unbounded_resources,
            views.unbounded_pile
        );
    }

    // (4) THE STORE IS UNTOUCHED — the projection filtered, it did not mutate.
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&marked),
        "the ∞ store must survive the projection (CR 104.4b / CR 110.1 lockstep + the \
         zone-exit defuse still need it until the boundary)"
    );
    assert!(
        !state.unbounded_loop_pile.is_empty(),
        "the ∞ pile must survive the projection too"
    );
}

/// R6a-3 (FAIL-CLOSED). An ∞ axis the accept marked but NO registered materialization
/// collapses must keep rendering its badge — the filter is keyed on what is actually
/// scheduled, never on what is merely *labellable*.
///
/// This is the row that kills the lazy-but-unsound filter: `LoopCollapseAxis`
/// `from_resource_axis` maps `TokensCreated` / `Counter(..)` / `Life(..)` to a label, so
/// building the hide-set from "does this axis have a collapse label" is a one-liner that
/// passes R6a-1 and silently hides an axis nothing will ever collapse.
///
/// REVERT-PROBE (RUN): build the projection filter from
/// `LoopCollapseAxis::from_resource_axis(axis).is_some()` instead of from
/// `scheduled_collapse_axes` ⇒ this row's `TokensCreated` badge vanishes ⇒ FAILS, while
/// R6a-1 still passes.
#[test]
fn unregistered_axis_still_renders_its_infinity_badge() {
    let mut state = r6a_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 200);

    // Keep the marks, DROP the registrations: the exact shape of an axis that is
    // collapsible-LABELLED but has nothing scheduled to collapse it.
    let marked = state
        .unbounded_resources
        .get(&P0)
        .expect("accept marked the ∞ axes")
        .clone();
    assert!(
        marked.contains(&ResourceAxis::TokensCreated) && marked.contains(&ResourceAxis::Life(P0)),
        "reach-guard: both labellable axes are marked, got {marked:?}"
    );
    // Positive control on the SAME state, BEFORE the drop: with the stash present the rows
    // are hidden, so the flip below is attributable to the missing registration alone.
    assert!(
        engine::game::derived_views::derive_views(&state, None)
            .unbounded_resources
            .is_empty(),
        "control: with the stash present the scheduled rows are hidden"
    );
    state.pending_unbounded_materialization.clear();

    let rows = engine::game::derived_views::derive_views(&state, None).unbounded_resources;
    let axes: Vec<ResourceAxis> = rows.iter().map(|r| r.axis).collect();
    assert!(
        axes.contains(&ResourceAxis::TokensCreated),
        "FAIL-CLOSED: a collapsible-LABELLED axis with NO registered materialization is \
         still unbounded and must keep its ∞ badge, got {axes:?}"
    );
    assert!(
        axes.contains(&ResourceAxis::Life(P0)),
        "FAIL-CLOSED: same for the life axis, got {axes:?}"
    );
}

/// R4-C4b (CR 732.2c). "Once the last player has either accepted or shortened the shortcut
/// proposal, the shortcut is taken" — its ending point is fixed at the accepted N, so the
/// CR 500.5 boundary collapse prompt may not offer a WIDER range than the table agreed to.
/// BASE re-asked with `max = MAX_SHORTCUT_CYCLES` (1000), letting a controller who proposed
/// 7 cycles walk away with 1000.
///
/// REVERT-PROBE (RUN): restore `max: crate::game::engine::MAX_SHORTCUT_CYCLES` ⇒ `max`
/// reads 1000 ⇒ FAILS. `min: 0` is asserted unchanged (a collapse-to-nothing stays legal).
#[test]
fn accepted_fixed_count_bounds_the_boundary_collapse_prompt() {
    let mut state = r6a_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 7);
    assert!(
        state.pending_unbounded_materialization.contains_key(&P0),
        "reach-guard: the accept scheduled a collapse, so the boundary WILL prompt"
    );

    r6a_drive_to_boundary(&mut state);

    match &state.waiting_for {
        WaitingFor::PayAmountChoice {
            player,
            resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
            min,
            max,
            ..
        } => {
            assert_eq!(*player, P0, "the loop controller is prompted");
            assert_eq!(
                *max, 7,
                "CR 732.2c: the accepted Fixed(7) bounds the collapse prompt (BASE: 1000)"
            );
            assert_eq!(*min, 0, "a collapse-to-nothing stays legal");
        }
        other => {
            panic!("the CR 500.5 boundary must prompt P0 for the collapse count, got {other:?}")
        }
    }

    // REJECTION DISCRIMINATOR — the bound is ENFORCED by the reducer, not merely advertised
    // in the prompt. This is the control a widened-`max` BASE cannot pass: with
    // `max = MAX_SHORTCUT_CYCLES` a submit of 8 is ACCEPTED, so this assertion is what makes
    // the range assertion above load-bearing rather than cosmetic.
    let over = apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 8 });
    assert!(
        matches!(&over, Err(EngineError::InvalidAction(msg)) if msg.contains("[0, 7]")),
        "CR 732.2c: collapsing PAST the accepted count must be rejected, got {over:?}"
    );

    // The bound is honored end-to-end: submitting exactly N is still accepted.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 7 })
        .expect("collapsing at exactly the accepted count is legal");
}

/// R6a FIX-2 (CR 732.2c). MEASURED DEFECT in the first cut of the collapse bound: the stash
/// `register_pending_materialization` APPENDS ("two accepts by the same controller, coexist"),
/// but the bound was written with a bare `insert`, i.e. it OVERWROTE. A controller who accepts
/// `Fixed(1)` and then, in the SAME phase, accepts `Fixed(1000)` therefore ends up with a
/// two-item stash bounded at 1000 — and since the boundary applies ONE submitted amount to
/// EVERY item, the first accept's loop would materialize 1000 times though the table agreed to
/// exactly one.
///
/// SHIPPED SEMANTICS, PINNED HERE EXPLICITLY: the bound is the MINIMUM of the accepted counts.
/// The second accept's agreed 1000 is UNDER-delivered down to 1. That is still a divergence
/// from what the table agreed to — but it is the safe polarity: no accept in the stash can ever
/// be over-materialized, which is the CR 732.2c violation ("the shortcut is taken" at the count
/// the last player accepted, not at some later, larger one). The exact per-accept bound needs
/// the flat stash to become accept-grouped and is deliberately NOT smuggled in here; the
/// boundary's pause-safety `sort_by_key` reorders that flat list, so a positional parallel
/// bound vector is not a valid shortcut to it.
///
/// REVERT-PROBE (RUN): restore `pending_materialization_count.insert(proposal.proposer, n)` in
/// `materialize_fixed_shortcut` ⇒ the bound reads 1000, the prompt offers `max == 1000`, and the
/// out-of-range submit is ACCEPTED ⇒ assertions (4), (5) and (6) FAIL.
#[test]
fn two_accepts_in_one_phase_bound_the_collapse_to_the_smallest_accepted_count() {
    let mut state = r6a_offer_state();

    // (1) reach-guard: the first real cast reached the CR 732.2a offer.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the first buyback+convoke recast must surface P0's offer, got {:?}",
        state.waiting_for
    );
    let phase_at_first_accept = state.phase;
    r6a_declare_and_accept_all(&mut state, P0, 1);
    assert_eq!(
        state.pending_materialization_count.get(&P0).copied(),
        Some(1),
        "the first accept records its own Fixed(1) bound"
    );

    // (2) The buyback returned Sprout Swarm to hand and priority came back, so a SECOND real
    // cast is available in the SAME phase — this is what makes the append reachable at all.
    let fodder = *state
        .battlefield
        .iter()
        .find(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == P0 && !o.tapped && o.name.contains("Saproling"))
        })
        .expect("an untapped P0 Saproling remains to convoke the second cast");
    let mut state = GameRunner::from_state(state)
        .cast(R6A_SPROUT)
        .accept_optional()
        .convoke_with(&[fodder])
        .commit()
        .resolve()
        .state()
        .clone();

    // (3) reach-guard: the second cast really produced a second offer, in the same phase.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the second recast must surface a second offer, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.phase, phase_at_first_accept,
        "both accepts land in ONE phase, so they share ONE stash and ONE boundary prompt"
    );
    r6a_declare_and_accept_all(&mut state, P0, 1000);

    // (4) THE PREMISE + THE FIX. The stash APPENDED (two items, one boundary amount for both),
    // and the bound is the MINIMUM — not the latest write.
    assert_eq!(
        state
            .pending_unbounded_materialization
            .get(&P0)
            .map(Vec::len),
        Some(2),
        "premise: the two accepts coexist in ONE stash, so ONE amount will scale BOTH"
    );
    assert_eq!(
        state.pending_materialization_count.get(&P0).copied(),
        Some(1),
        "CR 732.2c: min(1, 1000) — the later Fixed(1000) may NOT re-scale the Fixed(1) accept \
         (BASE overwrite: 1000)"
    );

    let p0_permanents = |s: &GameState| {
        s.battlefield
            .iter()
            .filter(|id| s.objects.get(id).is_some_and(|o| o.controller == P0))
            .count()
    };
    let permanents_before = p0_permanents(&state);
    let life_before = state.players.iter().find(|p| p.id == P0).unwrap().life;

    r6a_drive_to_boundary(&mut state);

    // (5) The prompt advertises the minimum.
    let WaitingFor::PayAmountChoice {
        player,
        resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
        max,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "the CR 500.5 boundary must prompt P0 for the collapse count, got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(*player, P0, "the loop controller is prompted");
    assert_eq!(
        *max, 1,
        "CR 732.2c: the prompt is bounded by the SMALLEST accepted count (BASE: 1000)"
    );

    // (6) And the reducer ENFORCES it — the second accept's agreed 1000 is unreachable.
    let over = apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 1000 });
    assert!(
        matches!(&over, Err(EngineError::InvalidAction(msg)) if msg.contains("[0, 1]")),
        "CR 732.2c: the later accept's 1000 cannot be collapsed at, got {over:?}"
    );

    // (7) WHAT B'S 1000 ACTUALLY BECOMES: exactly 1. Each of the two stashed sequences replays
    // ONCE — one new token and one life per sequence — so the first accept keeps precisely the
    // single cycle the table agreed to, and the second is capped down to the same.
    //
    // NOT the BASE discriminator, and deliberately not claimed as one: this submits
    // `amount: 1`, which the BASE overwrite ALSO materializes as Δ2. A bare-`insert` revert
    // probe was RUN and fails only at assertion (5) (`left: Some(1000)`). What BASE gets
    // wrong is that it ADVERTISES `max: 1000` and PERMITS a 1000× submit — assertions (4),
    // (5) and (6) are the rows that catch that. This row exists to pin the post-collapse
    // board, i.e. that the enforced bound is also the delivered one.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 1 })
        .expect("collapsing at the minimum accepted count is legal");
    assert_eq!(
        p0_permanents(&state) - permanents_before,
        2,
        "one materialized cycle per stashed accept, never 1000"
    );
    assert_eq!(
        state.players.iter().find(|p| p.id == P0).unwrap().life - life_before,
        2,
        "same for the life axis: one cycle per stashed accept"
    );
}

/// R6a FIX-4 (CR 732.2c). The AI's `LoopCollapse` candidate was a hardcoded `amount: 1`, from
/// when the prompt's `max` was the fixed engine-wide `MAX_SHORTCUT_CYCLES`. Binding `max` to
/// the accepted count makes `max == 0` reachable — a shortcut everyone accepted at `Fixed(0)`
/// — and the reducer rejects `amount > max`, so the generator's SOLE candidate would be
/// illegal and an AI-seated controller would have no legal action at this prompt.
///
/// Driven end-to-end: a real cast → a real `Fixed(0)` declaration → real APNAP accepts → the
/// real CR 500.5 boundary prompt → the production `ai_support::legal_actions` generator → the
/// production `apply()` reducer.
///
/// REVERT-PROBE (RUN, MEASURED): restore `GameAction::SubmitPayAmount { amount: 1 }` in
/// `ai_support::candidates` ⇒ `legal_actions` returns `[]`. `legal_actions` validates its
/// candidates against the reducer, so the illegal `amount: 1` is not merely rejected on
/// submit — it is dropped, leaving the AI with NO legal action at this prompt. Assertion (3)
/// FAILS (`left: []`).
#[test]
fn ai_collapse_candidate_is_clamped_to_the_accepted_bound() {
    let mut state = r6a_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 0);
    r6a_drive_to_boundary(&mut state);

    // (1) reach-guard: a `Fixed(0)` accept really does register a stash and really does prompt.
    // (2) ...with the zero-width range the clamp exists for.
    let WaitingFor::PayAmountChoice {
        resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
        min,
        max,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "reach-guard: a Fixed(0) accept must still reach the boundary prompt, got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(
        (*min, *max),
        (0, 0),
        "CR 732.2c: Fixed(0) bounds the prompt to exactly 0"
    );

    // (3) The production candidate generator offers the clamped amount (BASE: a hardcoded 1).
    let candidates = engine::ai_support::legal_actions(&state);
    assert_eq!(
        candidates,
        vec![GameAction::SubmitPayAmount { amount: 0 }],
        "the AI's sole collapse candidate is clamped to the accepted bound"
    );

    // (4) ...and it is actually LEGAL — the assertion that makes (3) load-bearing rather than
    // a restatement of the generator.
    apply(&mut state, P0, candidates[0].clone())
        .expect("the AI's generated candidate must be accepted by the reducer");
}
