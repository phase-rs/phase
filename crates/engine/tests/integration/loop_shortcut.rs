// engine-citation-gate: symbol anchors only
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
    AnnouncementSubject, DecisionGroupKey, DecisionKind, DecisionPoint, DecisionPointKind,
    DecisionSlot, DecisionTemplate, IterationCount, PinnedDecision, Ranking, ReplayMode,
    ShortcutDecisionSchema, TargetPin, TargetSchedule,
};
use engine::analysis::loop_check::{LoopCertificate, ShortcutProposal, ShortcutResponse, WinKind};
use engine::analysis::resource::{loop_states_equal_modulo_resources, BoardDelta, ResourceAxis};
use engine::game::derived_views::{FamilyCollapseState, UnboundedFamily};
use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassRequest, CastPaymentMode, GameState, LoopDetectionMode, StackEntryKind, WaitingFor,
    YieldTarget,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// The one-element ranking a schedule step carries when it names a single object: a
/// `Ranking::one(Object(src))` IS the pre-parameterization constant subject, so every site
/// below is a re-spelling with no behaviour delta (CR 732.2a — only the head is ever
/// resolved within one drive).
fn obj_rank(src: YieldTarget) -> Ranking {
    Ranking::one(AnnouncementSubject::Object(src))
}

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
const GOLDEN_ON: &str = r#"[StackPushed { object_id: ObjectId(3) }, ZoneChanged { object_id: ObjectId(3), from: Some(Hand), to: Stack, record: ZoneChangeRecord { object_id: ObjectId(3), name: "Test Lifegain Kickoff", core_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], trigger_definitions: [], trigger_source_context: Some(TriggerSourceContext { identity: ObjectIdentityBinding { reference: ObjectIncarnationRef { object_id: ObjectId(3), incarnation: 0 }, expected_zone: Hand }, lki: LKISnapshot { name: "Test Lifegain Kickoff", token_image_ref: None, power: None, toughness: None, base_power: None, base_toughness: None, mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), card_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], colors: [], chosen_attributes: [], counters: {}, tapped: false, is_suspected: false, attachments: [] }, card_id: CardId(3), printed_ref: None, is_token: false, face_down: false, transformed: false, is_renowned: false, is_saddled: false, class_level: None, trigger_entries: [], timestamp: 0, entered_battlefield_turn: None, paired_with: None, pair_controller: None, attached_to: None, attachments: [], linked_exile_snapshot: [], combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, cast_from_zone: None, played_from_zone: None, cast_controller: None, phase_status: PhasedIn, cast_variant_paid: None, cast_timing_permission: None, cost_x_paid: None, cast_spell_keywords: [], mana_spent_to_cast: false, colors_spent_to_cast: ColoredManaCount { white: 0, blue: 0, black: 0, red: 0, green: 0 }, mana_spent_to_cast_amount: 0, kickers_paid: [], additional_cost_payment_count: 0, additional_cost_payments: [], cast_cost_paid_object: None }), power: None, toughness: None, base_power: None, base_toughness: None, colors: [], mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), from_zone: Some(Hand), cast_from_zone: None, played_from_zone: None, to_zone: Stack, attachments: [], linked_exile_snapshot: [], is_token: false, combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, co_departed: [], entered_incarnation: None, attached_to: None, turn_zone_change_index: 0, recorded_turn_number: 2, is_suspected: false } }, SpellCast { card_id: CardId(3), controller: PlayerId(0), object_id: ObjectId(3) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(0), amount: 1 }, EffectResolved { kind: GainLife, source_id: ObjectId(3), subject: None }, ZoneChanged { object_id: ObjectId(3), from: Some(Stack), to: Graveyard, record: ZoneChangeRecord { object_id: ObjectId(3), name: "Test Lifegain Kickoff", core_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], trigger_definitions: [], trigger_source_context: Some(TriggerSourceContext { identity: ObjectIdentityBinding { reference: ObjectIncarnationRef { object_id: ObjectId(3), incarnation: 1 }, expected_zone: Stack }, lki: LKISnapshot { name: "Test Lifegain Kickoff", token_image_ref: None, power: None, toughness: None, base_power: None, base_toughness: None, mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), card_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], colors: [], chosen_attributes: [], counters: {}, tapped: false, is_suspected: false, attachments: [] }, card_id: CardId(3), printed_ref: None, is_token: false, face_down: false, transformed: false, is_renowned: false, is_saddled: false, class_level: None, trigger_entries: [], timestamp: 0, entered_battlefield_turn: None, paired_with: None, pair_controller: None, attached_to: None, attachments: [], linked_exile_snapshot: [], combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, cast_from_zone: None, played_from_zone: None, cast_controller: None, phase_status: PhasedIn, cast_variant_paid: None, cast_timing_permission: None, cost_x_paid: None, cast_spell_keywords: [], mana_spent_to_cast: false, colors_spent_to_cast: ColoredManaCount { white: 0, blue: 0, black: 0, red: 0, green: 0 }, mana_spent_to_cast_amount: 0, kickers_paid: [], additional_cost_payment_count: 0, additional_cost_payments: [], cast_cost_paid_object: None }), power: None, toughness: None, base_power: None, base_toughness: None, colors: [], mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), from_zone: Some(Stack), cast_from_zone: None, played_from_zone: None, to_zone: Graveyard, attachments: [], linked_exile_snapshot: [], is_token: false, combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, co_departed: [], entered_incarnation: None, attached_to: None, turn_zone_change_index: 1, recorded_turn_number: 2, is_suspected: false } }, StackResolved { object_id: ObjectId(3) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(1), amount: -1 }, EffectResolved { kind: LoseLife, source_id: ObjectId(1), subject: None }, StackResolved { object_id: ObjectId(4) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(0), amount: 1 }, EffectResolved { kind: GainLife, source_id: ObjectId(2), subject: None }, StackResolved { object_id: ObjectId(5) }, GameOver { winner: Some(PlayerId(0)) }]"#;

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
/// IMMUNE to life loss (CR 101.2 — a "can't" effect takes precedence over the trigger's
/// life-loss instruction; cf. CR 119.8, which governs only life EXCHANGES, life
/// REDISTRIBUTION and pay-life COSTS, none of which happens here). So the cycle drains ONLY P1 (sole
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
/// instruction; cf. CR 119.8, which governs only life EXCHANGES, life REDISTRIBUTION and
/// pay-life COSTS, none of which happens here). Living partition each
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
///
/// # PR-7 Phase 5b — the DECLINE arm, and why the guard would otherwise hollow out
///
/// These boards now raise a natural bounded CR 732.2a offer. A `WaitingFor::LoopShortcut` is
/// neither `Priority` nor `OrderTriggers`, so without the arm below the break test fires at
/// the offer and the drive ends BEFORE recurrence can be witnessed — every caller's `primed`
/// reach-guard would then report `false` on a perfectly primed loop, i.e. the guard fails for
/// a reason that has nothing to do with what it guards.
///
/// The remedy re-grounds each guard THROUGH the offer rather than around it: decline and keep
/// driving. Declining is a PASS-THROUGH, not an assertion — these rows' claims are about the
/// CROWN, not about the offer — and the declined offers are returned so a caller that wants to
/// assert on one can (`.first()`), and so the number of declines is reportable (`.len()`).
///
/// MEASURED CONSEQUENCE: `DeclineShortcut` is a deliberate action and invalidates the ring
/// (`apply_action`'s deliberate-action ring invalidation), so the recurrence witness must be
/// re-accumulated after each decline and the caps tuned against an un-cleared ring no longer
/// hold. See `PRIMED_LOOP_BEATS`.
fn drive_collect_primed(
    runner: &mut GameRunner,
    cap: usize,
) -> (Vec<GameEvent>, WaitingFor, bool, Vec<WaitingFor>) {
    let mut all: Vec<GameEvent> = Vec::new();
    let mut primed = false;
    let mut declined: Vec<WaitingFor> = Vec::new();
    for _ in 0..cap {
        // Two separate `matches!` guards, not one `match`: the first borrow ends before
        // `act` needs `&mut`.
        if matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }) {
            declined.push(runner.state().waiting_for.clone());
            let result = runner
                .act(GameAction::DeclineShortcut)
                .expect("DeclineShortcut is legal at a LoopShortcut window");
            all.extend(result.events);
            continue;
        }
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
                .any(|prior| loop_states_equal_modulo_resources(&prior.normalized, state));
        }
    }
    (all, runner.state().waiting_for.clone(), primed, declined)
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

/// SITE D (CR 732.2a) — the `UntilLethal` drive dispatch: a FOREIGN driving period in state must
/// not divert an accepted Path-A grant into the object-growth drive.
///
/// **WHY THIS ROW EXISTS NOW AND DID NOT BEFORE.** Site D was reported row-less on the ground that
/// no fixture reaches it with a foreign period. That was a statement about what boards ARRIVE
/// carrying one, not about reachability: `migrate_transient_loop_sequence` clears the field at
/// every load, so no dump-driven row can start from one, and the answer here is the same one the
/// mint and accept rows use — inject into a board the engine itself drove to its offer.
///
/// **WHY THIS SCENARIO AND NOT A CAPTURE.** Site D is only reachable through a proposal whose count
/// is `UntilLethal`, and `handle_declare_shortcut` rejects `UntilLethal` against any offer that
/// narrowed its bound. Every tracked capture in this repo reaches the BOUNDED mint (asserted on the
/// Dina capture by `the_user_captures_offer_is_reached_with_its_driving_period_cleared`), so the
/// only route in is a Path-A offer — which is exactly what
/// [`interactive_3p_optional_cascade_apnap_accept_win`] directly above raises. This row is that row
/// plus one injected field, so any divergence attributes to the field alone.
///
/// **THE HAZARD.** Under a merely-non-empty test, `apply_until_lethal_shortcut` would take its
/// object-growth branch and drive the FOREIGN seat's recorded period, measuring that seat's delta
/// as if it were this proposal's. CR 732.2a binds a shortcut to the sequence its proposer can
/// predictably take, and another seat's independent activation is not among them. Pre-existing and
/// independent of the (1b) fix — Path A never read the sequence — but reachable, and fixed through
/// the same authority.
///
/// **TWO-SIDED CONTROL** (both measured; each direction breaks a DIFFERENT row):
/// * **DROP** the seat test (restore `!committed.last_loop_action_sequence.is_empty()`) ⇒ this row
///   ends at `Priority { player: P0 }` instead of `GameOver { winner: Some(P0) }` — the drive
///   fell into `until_lethal_fallback` — and the injected period is wiped to length 0 by that
///   fallback's unconditional clear. BOTH assertions below flip.
/// * **TRIVIALIZE** to a constant `true` (always drive the recorded period) ⇒
///   `interactive_3p_optional_cascade_apnap_accept_win` above panics inside the engine at
///   `seq[0]`, because an EMPTY sequence has no step to drive. So no constant implementation of
///   this dispatch passes the pair.
///
/// ⚠ **A REALIZED NEGATIVE, recorded rather than hidden.** The complementary constant —
/// `false`, i.e. always take the drain branch — was measured against the WHOLE integration suite
/// at this tree (`cargo test -p phase-engine --test integration`, one unit = one libtest row) and
/// **4564 rows passed, 0 failed**. Site D's own-period branch is therefore asserted by no row
/// in this tree, which is a pre-existing coverage gap this change neither creates nor closes: the
/// object-growth `UntilLethal` rows (`object_growth_advantage_untillethal_no_crown`) reach the
/// same `Priority` handback down either branch, because `until_lethal_fallback` rolls the board
/// back to `committed` and the two routes become observationally identical.
#[test]
fn an_accepted_until_lethal_grant_drains_even_with_a_foreign_period_in_state() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ..
    } = wf.clone()
    else {
        panic!("REACH-GUARD: every assertion below needs the engine's own offer, got {wf:?}");
    };
    assert_eq!(
        predicted_winner,
        Some(P0),
        "REACH-GUARD: site D is reachable only through a Path-A offer — a bounded one rejects \
         `UntilLethal` at the declare seam and this row would measure that rejection instead"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("the proposer declares UntilLethal on the Path-A offer");

    // THE INJECTION: an opponent's own recorded period, sitting in state at the moment the last
    // acceptance hands the proposal to `apply_until_lethal_shortcut`.
    let opp = runner
        .state()
        .players
        .iter()
        .map(|p| p.id)
        .find(|p| *p != proposer)
        .expect("REACH-GUARD: the foreign period needs a second seat to belong to");
    let card_id = runner
        .state()
        .objects
        .values()
        .next()
        .map(|o| o.card_id)
        .expect("the scenario has objects");
    runner.state_mut().last_loop_action_sequence = vec![LoopActionContext {
        card_id,
        controller: opp,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::NotUsed,
        },
        convoke: None,
        pins: vec![],
    }];
    assert_ne!(
        opp, proposer,
        "REACH-GUARD: a period injected for the PROPOSER would be the legitimate object-growth \
         route, and this row would assert the opposite of what it means to"
    );

    accept_all_opponents(&mut runner);

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "CR 732.2a SITE D: an opponent's recorded activation describes no sequence this proposer \
         can take, so the accepted `UntilLethal` grant must still drive the DRAIN it was certified \
         on. A `Priority` handback here is the defect: the drive took the object-growth branch and \
         measured the wrong seat's period"
    );
    assert_eq!(
        runner.state().last_loop_action_sequence.len(),
        1,
        "and the foreign period is still THERE — the crown was reached with it in state. Under the \
         DROP mutant this reads 0, because `until_lethal_fallback` clears the field \
         unconditionally, so a wrongly-routed drive also destroys the other seat's period"
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
    // `(waiting_for, action)` match; `game/engine.rs`'s `check_actor_authorization` runs BEFORE
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
        per_cycle: None,
    };
    let wf_a = WaitingFor::LoopShortcut {
        proposer: P1,
        predicted_winner: Some(P0),
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
    };
    let wf_b = WaitingFor::LoopShortcut {
        proposer: P2,
        predicted_winner: None,
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
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
        per_cycle: None,
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
        declaration: None,
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
///
/// PR-7 Phase 5b — the cap was RE-MEASURED, not re-derived. `DeclineShortcut` is a deliberate
/// action and invalidates the ring, so the decline arm [`drive_collect_primed`] gained could in
/// principle have pushed the recurrence witness past this cap. Measured on this tree: it does
/// not — all three rows below still witness recurrence at 24 with the arm live, so the swept
/// value stands unchanged. The arm's liveness is not assumed either: deleting it drops all
/// three to `primed == false` / "0 bounded offers declined".
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
///
/// PR-7 Phase 5b: this class now also raises a bounded CR 732.2a offer, so the row's former
/// "must NOT raise a LoopShortcut offer" clause is superseded and replaced by a positive
/// discriminator on `predicted_winner`. See the comment at that assertion. Two further
/// revert-probes, each flipping a DIFFERENT assertion so neither dominates the other:
/// * make `try_offer_bounded_cycle_shortcut` refuse unconditionally ⇒ `declined` is empty ⇒
///   the `offered` `let-else` panics, while both no-CROWN assertions stay green.
/// * delete the `DeclineShortcut` arm from [`drive_collect_primed`] ⇒ the drive breaks at the
///   offer ⇒ `primed == false` ⇒ the trailing reach-guard FAILS.
#[test]
fn interactive_3p_subset_lethal_does_not_crown() {
    let (mut runner, kickoff) = setup_3p_subset_lethal(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (events, wf, primed, declined) = drive_collect_primed(&mut runner, PRIMED_LOOP_BEATS);

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
    // PR-7 Phase 5b — the SUPERSEDED clause, replaced by its positive discriminator.
    //
    // This row used to assert `!matches!(wf, WaitingFor::LoopShortcut { .. })`. That
    // expectation is superseded, not violated: a subset-lethal drain that crowns nobody is
    // the exact class the bounded CR 732.2a offer exists to serve, so the class now OFFERS.
    // The no-CROWN claims above are unchanged and are still the row's soundness content.
    //
    // Deleting a negative assertion silently would leave the row weaker than it was, so it is
    // replaced by a POSITIVE assertion on the field that separates the two classes: a bounded
    // offer must carry `predicted_winner: None`. A `Some(winner)` here would mean the offer
    // came from Path A — i.e. something DID crown after all — which is exactly what the two
    // assertions above forbid.
    let offered = declined.first();
    let Some(WaitingFor::LoopShortcut {
        predicted_winner,
        certificate,
        ..
    }) = offered
    else {
        panic!(
            "PR-7 5b: this class now OFFERS the bounded shortcut, and the drive above declined \
             every offer it saw; got {} declined offers, terminal {wf:?}",
            declined.len()
        );
    };
    assert_eq!(
        *predicted_winner, None,
        "the offer must carry predicted_winner: None — a Some(winner) here would mean Path A \
         crowned after all, contradicting the CR 104.2a assertions above"
    );
    // BASIS: measured **A** (direct recurrence) — instrumenting the `basis_a` match in
    // `try_offer_bounded_cycle_shortcut` prints `BASIS=A turn=2 phase=PreCombatMain ring=3` at
    // this row's offer beat, which is consistent with `primed` below witnessing exactly basis
    // A's first disjunct. `ring_delta_signature` — the only function the CR 703.1
    // turn-position conjunct modifies — is reached ONLY from the `None =>` arm, so this row is
    // orthogonal to that conjunct in both directions.
    //
    // HONEST SCOPE OF THE ASSERTION BELOW: `frames_per_period` is a TRIPWIRE on the period's
    // WIDTH, not a proof of basis. Basis B derives `k` from 1 upward, so a k==1 basis-B offer
    // publishes the same value as a 1-frame basis-A one (the
    // `dina_untargeted_drain_4p_offers_at_three_live_opponents` row is exactly that case).
    //
    // ⚠ VALUE CORRECTED 1 → 2. The NUMBER is the small half of the correction; the MECHANISM
    // is the half worth carrying forward, because it names a class of defect rather than one
    // fixture's constant.
    //
    // THIS ASSERTION WAS A SELF-RATIFYING ORACLE. Basis A published a HARDCODED
    // `frames_per_period: 1` regardless of how far back its certifying prior actually sat. So
    // the assertion compared a literal `1` against a constant `1` that no game state could
    // influence: it could not fail for ANY fixture, ANY period width, or ANY future change to
    // how the ring is sampled. It read the implementation's constant back to itself and
    // reported that as agreement. Its stated premise — "this class certifies on a single-frame
    // period" — was therefore never measured; it was inferred from the very line it was
    // supposedly checking.
    //
    // That is the same family as this lane's other "guard that passes while proving nothing"
    // findings: a check whose subject cannot vary is not a check. The tell is available
    // WITHOUT running anything — trace the asserted expression back to its producer and ask
    // whether any input can move it. If nothing can, the row's green is a tautology.
    //
    // The mint now DERIVES the span from the prior's ring index, so the expression varies with
    // the fixture. For this one it is 2: the DRAIN_CLERIC / BLOOD_SIPPER pairing alternates a
    // gain-life resolution and a lose-life resolution, so one whole repetition spans two
    // retained ring frames and the pair one frame back does not recur.
    //
    // MEASURED through the production accept path on this same fixture (declare `Fixed(n)` +
    // APNAP accepts), which is what makes 2 the RIGHT value rather than merely a different one:
    //   derived k = 2 ⇒ n=1 → δ{P0:+1,P1:-1}; n=2 → +2/-2; n=3 → +3/-3   (exactly n × δ)
    //   hardcoded   1 ⇒ n=1, 2, 3 → ZERO committed, every time
    // That zero is `materialize_fixed_shortcut`'s conformance check doing its job: a cycle cut
    // at one frame delivers half a period, which does not equal the published δ, so the drive
    // drops it and hands back. A wrong period is therefore never a silent half-commit — but it
    // does make the offer unusable, which is why the span has to be measured and not assumed.
    assert_eq!(
        certificate
            .per_cycle
            .as_ref()
            .expect("a bounded offer publishes its per-period signature")
            .frames_per_period,
        2,
        "this class's repetition spans two retained ring frames (a gain-life resolution, then a \
         lose-life resolution); a drift in that width silently changes what one committed cycle \
         means"
    );

    // Reach-guard on the regime itself (see `drive_collect_primed`): the drive really did reach
    // a board-recurrent state, so `live_mandatory_loop_winner` ran on the subset-lethal loop the
    // assertions above are about — they refused a real classification rather than never posing
    // one. Deliberately LAST here: this test's classification and its wrongful-crown failure
    // mode live in the same drive, so under the weakened-gate defect the crown must report as a
    // crown (the `wf` assertion above) — measured, a guard placed first steals that panic (M1
    // crowns at beat ~1, before recurrence) and reports the wrong cause.
    //
    // RE-GROUNDED THROUGH the offer, not around it: `DeclineShortcut` invalidates the ring, so
    // the witness must be re-accumulated after each decline, which is why the decline count is
    // reported here.
    assert!(
        primed,
        "the loop never reached a board-recurrent state within {PRIMED_LOOP_BEATS} beats \
         ({} bounded offers declined en route) — this is not the primed-loop regime the \
         assertions above assume, so they passed vacuously",
        declined.len()
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
        per_cycle: None,
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
///
/// PR-7 Phase 5b: the leading reach-guard is re-grounded THROUGH the bounded offers this class
/// now raises (see [`drive_collect_primed`]). REVERT-PROBE (MEASURED, not predicted): delete
/// that decline arm ⇒ `primed == false` ⇒ this row FAILS with "0 bounded offers declined".
#[test]
fn injected_3p_one_faller_no_crown() {
    let (mut runner, kickoff) = setup_3p_subset_lethal(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, _wf, primed, declined) = drive_collect_primed(&mut runner, PRIMED_LOOP_BEATS);

    // Reach-guard on the regime (see `drive_collect_primed`): the board reached a genuine
    // recurrence, so the E1 clone-drive below has a real cycle to measure rather than an
    // un-primed board. (It witnesses the live bridge's frame pair, not the E1 measure's own
    // boundary/work pair — those are different frames.)
    //
    // PR-7 Phase 5b: RE-GROUNDED THROUGH the bounded offers this class now raises — the driver
    // declines each one and keeps driving, so the witness still ranges over a non-empty beat
    // set. `DeclineShortcut` invalidates the ring, hence the decline count in the message.
    assert!(
        primed,
        "the loop never reached a board-recurrent state within {PRIMED_LOOP_BEATS} beats \
         ({} bounded offers declined en route) — the E1 measure below would have no primed \
         cycle, so its no-crown assertion is vacuous",
        declined.len()
    );

    // Reach-guard: the drain loop genuinely ran (P1 bled, alive) and P2 is untouched — this
    // is the subset-lethal regime the E1 measure must refuse.
    assert!(
        life(&runner, P1) < 1000 && !is_eliminated(&runner, P1),
        "P1 must have bled (loop primed), life = {}",
        life(&runner, P1)
    );
    assert_eq!(life(&runner, P2), 20, "P2 untouched (second non-faller)");

    // Inject the offer, then confirm it. PR-7 Phase 5b: this board is now ALSO reachable
    // naturally (the drive above declined its natural bounded offers), but the injection stays
    // — the injected `predicted_winner: Some(P0)` + `UntilLethal` certificate is what pins the
    // E1 declare path this row is about, and the natural offer is a `None`/`Fixed` one that
    // would route somewhere else entirely.
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
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
    // MEASURED, not assumed (PA-2B.0b was a HYPOTHESIS that the materialized settle would now
    // raise a natural bounded offer here and turn this green assertion red): on this tree the
    // post-settle state IS `Priority`, so the assertion stands VERBATIM and needs no
    // decline-and-re-read pass-through. Do not relax it to an `||` over two `WaitingFor`
    // variants — that would make it pass on a state this row exists to exclude.
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
        // No narrowed CR 732.2a bound — `Default` carries the global cap.
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
        declaration: None,
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
        declaration: None,
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
///
/// PR-7 Phase 5b: this class now raises a natural bounded CR 732.2a offer mid-drive, which
/// would end the drive before the recurrence witness accumulates. [`drive_collect_primed`]
/// declines it and keeps driving, so the reach-guard is grounded THROUGH the offer.
/// REVERT-PROBE (MEASURED, not predicted): delete that decline arm ⇒ the UNEQUAL half's
/// `primed` goes false and this row FAILS, while the equal half is untouched (it consumes 0
/// beats) — so the probe flips exactly one half, which is the proof the two halves are
/// independently grounded.
#[test]
fn injected_3p_unequal_life_pin_all_no_crown() {
    // Drive one primed cycle of a confirmed 3p both-fall drain and report the terminal
    // waiting_for.
    fn drive_confirmed(p1_life: i32, p2_life: i32) -> (WaitingFor, bool, usize) {
        let (mut runner, kickoff) =
            setup_3p_both_fall(LoopDetectionMode::Interactive, p1_life, p2_life);
        let _ = runner.cast(kickoff).resolve();
        let (_events, _wf, primed, declined) = drive_collect_primed(&mut runner, PRIMED_LOOP_BEATS);
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
            declaration: None,
        };
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::UntilLethal,
                template: None,
            })
            .expect("P0 declares UntilLethal");
        accept_all_opponents(&mut runner);
        (runner.state().waiting_for.clone(), primed, declined.len())
    }

    // UNEQUAL absolute life (gap 50) ⇒ NO crown (F2 staggered-death veto).
    let (unequal, unequal_primed, unequal_declines) = drive_confirmed(1000, 1050);
    // Reach-guard on the regime (see `drive_collect_primed`) — asserted on THIS half only: the
    // equal-life half below is measured to consume 0 beats (already crowned as the kick-off
    // resolved), so it has no drive in which to recur. This is the half the F2 revert-probe
    // flips, so the whole discriminator lives on the guarded side.
    //
    // PR-7 Phase 5b: RE-GROUNDED THROUGH the bounded offers this class now raises — declined,
    // not avoided. Only the unequal half is re-grounded, for the reason above.
    assert!(
        unequal_primed,
        "the loop never reached a board-recurrent state within {PRIMED_LOOP_BEATS} beats \
         ({unequal_declines} bounded offers declined en route) — this is not the ≥2-faller \
         primed regime the assertions below assume"
    );
    assert!(
        !matches!(unequal, WaitingFor::GameOver { winner: Some(_) }),
        "unequal-life ≥2-faller drain must NOT crown (CR 704.3 simultaneity), got {unequal:?}"
    );
    // MEASURED, not assumed (PA-2B.0b was a HYPOTHESIS that the materialized settle would now
    // raise a natural bounded offer here and turn this green assertion red): on this tree the
    // post-settle state IS `Priority`, so the assertion stands VERBATIM and needs no
    // decline-and-re-read pass-through. Do not relax it to an `||` over two `WaitingFor`
    // variants — that would make it pass on a state this row exists to exclude.
    assert!(
        matches!(unequal, WaitingFor::Priority { .. }),
        "the F2 veto hands back to manual play, got {unequal:?}"
    );

    // EQUAL absolute life ⇒ CROWN (reach-guard: the F2 check is not always-reject).
    let (equal, _, _) = drive_confirmed(1000, 1000);
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
    let mut schedule = vec![(0u32, obj_rank(valid.clone()))];
    if let Some(at) = switch_to_bogus_at {
        schedule.push((at, obj_rank(bogus)));
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
/// sailing through the WS frame cap (`phase-server`'s `MAX_WS_MESSAGE_BYTES`, 64 KB). The count
/// is HARDCODED as
/// `Fixed(u32::MAX)`; the cap
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
/// `Priority{active_player}` beat CLEARS `loop_detect_ring` (`game/engine.rs`'s
/// `pass_priority_once_with_pipeline` sample-or-clear arm). So the ring never accumulates, the
/// `!ring.is_empty()` bridge gate in `reconcile_terminal_result` never passes, and
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
///     draw gate CERTIFIES a benign no-loss loop: it draws `GameOver{None}` via
///     `interactive_loop_bridge`'s `has_no_loss_axis` draw arm — the one that pushes
///     `GameEvent::GameOver { winner: None }` (measured P0 life 22, cycle ~2; and neutering that
///     arm makes this control STOP drawing — confirmed the draw originates AT that gate, not at
///     the stricter arm of the same fn, which additionally requires
///     `classify_win_kind(..) == WinKind::Advantage` and emits no `GameOver` at all).
///   - VARIANT (`setup_3p_poison_draw`, IDENTICAL + a poison-rider creature) locks that a
///     poison-accruing loop is NOT wrongly drawn: it resolves via the CR 704.5c 10-poison SBA
///     to `GameOver{Some(P0)}` (measured P0 life 30, poisons [0,10,10], both opponents
///     eliminated).
///
/// SCOPE (measured — do NOT overclaim): this does NOT isolate `has_no_loss_axis`'s Path-B
/// conjunct. That conjunct IS load-bearing BY CONSTRUCTION (it is the SOLE loss-axis veto in that
/// draw arm, which has NO `== Advantage` backstop — a poison loop that reached the gate would
/// be wrongly drawn without it), but it is currently NOT runtime-discriminable, so there is NO
/// claim here that deleting it flips the variant. MEASURED: deleting `has_no_loss_axis` from
/// Path B leaves the variant terminal `GameOver{Some(P0)}` UNCHANGED — because the variant
/// never REACHES the gate with poison>0. A single-compound-trigger poison loop DOES reach the
/// Path-B bridge, but the "you gain N life and [each opponent gets a poison counter]" parser
/// drop removes the poison conjunct (card-build keeps only `GainLife`), so poison is 0 in the
/// loop delta at the gate → it draws as a benign lifegain loop and never exercises
/// has_no_loss_axis's poison veto. No constructible fixture carries poison>0 to the Path-B gate
/// (the 2-trigger form clears `loop_detect_ring` on its OrderTriggers beats, in
/// `pass_priority_once_with_pipeline`'s sample-or-clear arm;
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
/// `interactive_recurring_poison_is_not_drawn` above — by symbol; the line range that used to
/// follow it pointed at unrelated code even before this change moved it). The novel
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
/// optional-loop iteration seed EQUAL to the ceiling it publishes. Board-derivation (hostile): the creature TAPPED to
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
    // CR 732.2a + CR 732.2c: an optional Advantage loop narrows no CR 704 bound, so the offer
    // STATES the same global ceiling it publishes — the frontend echoes this value verbatim and
    // the accepted count caps the CR 500.5 collapse prompt, so a smaller seed would cap it too.
    assert_eq!(
        schema.iteration_count,
        IterationCount::Fixed(schema.max_iterations)
    );

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
        // No narrowed CR 732.2a bound — `Default` carries the global cap.
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
        per_cycle: None,
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: cert,
        schema,
        declaration: None,
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

/// R0-a ⭐ (SECURITY): the RESPONDER-facing copy of a declared template is redacted too.
///
/// `handle_declare_shortcut` moves the proposer's `DecisionTemplate` VERBATIM onto
/// `ShortcutProposal.template` one state transition after the `LoopShortcut` offer whose
/// `declaration` is redacted by `d5h_a_hidden_object_pin_drops_the_whole_declaration_for_a_non_proposer`
/// (`engine/src/game/visibility.rs`). Before this row, `grep -c RespondToShortcut
/// crates/engine/src/game/visibility.rs` was 0: the identical pin vector was public to every
/// responder and spectator.
///
/// CR 732.2b, ALL-OR-NOTHING: the responder's right is to shorten "by naming a place where they
/// will make a game choice that's different than what's been proposed", so a half-shown pin set
/// would state a proposal that was never made. The hostile fixture pins ONE hidden hand card and
/// ONE public seat, and the assertion is `template.is_none()` — an implementation that merely
/// trimmed the hidden pin would hand back `Some` with a one-pin vector and fail here.
///
/// # Non-vacuity
///
/// The board is FOUR seats so that the pinned card's owner is neither the proposer nor a
/// responder, which separates three properties a 2-seat fixture conflates. Paired positives,
/// because a redactor that simply dropped every template would satisfy the negative for free:
/// (1) the PROPOSER (P0) keeps it — the guard is keyed to the viewer boundary; (2) the OWNER of
/// the pinned hand card (P3) keeps it even though they are not the proposer — so the drop is
/// keyed to what the viewer may actually see, not to "everyone but the proposer"; (3) an all-seat
/// template reaches the responder unchanged and byte-equal to the proposer's. The negative is
/// asserted for the QUEUED responder P2 as well as the current one P1, which is what makes the
/// guard's keying on `proposal.proposer` (not on the prompted `player`) observable.
///
/// REVERT-PROBE: delete the `WaitingFor::RespondToShortcut` arm in `filter_state_for_viewer` ⇒
/// P1/P2 see the hidden-hand pin ⇒ the two `is_none()` assertions FAIL while all positives stay
/// green. Flipping the shared `pins_name_hidden_source`'s inner `any` to `all` fails this row AND
/// the pre-existing `LoopShortcut` row D5-h — which is the proof that the extraction left ONE
/// authority behind rather than two copies.
///
/// *What wrong implementation would still pass this row?* One that redacts `proposal.template`
/// but leaks the same identity through the `LoopShortcut` offer it came from — that surface is
/// covered by D5-h and by `loop_shortcut_schema_redacts_hidden_targets_for_non_controller` above.
#[test]
fn respond_to_shortcut_template_redacts_a_hidden_pin_for_non_proposers() {
    const P3: PlayerId = PlayerId(3);

    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // The pinned card sits in a hand belonging to NEITHER the proposer (P0) nor either
    // responder (P1 current, P2 queued), so "cannot see it" and "is not the proposer" are
    // distinguishable properties of a viewer on this one board.
    let hidden_hand = scenario.add_bolt_to_hand(P3);
    let runner = scenario.build();

    let source = YieldTarget::ThisObject {
        source_id: ObjectId(999),
        incarnation: None,
        trigger_description: None,
    };
    let window = |pins: Vec<TargetPin>| -> GameState {
        let mut state = runner.state().clone();
        state.waiting_for = WaitingFor::RespondToShortcut {
            player: P1,
            remaining_players: vec![P2],
            proposal: ShortcutProposal {
                proposer: P0,
                predicted_winner: Some(P0),
                count: IterationCount::Fixed(3),
                unbounded: vec![],
                win_kind: WinKind::LethalDamage,
                template: Some(DecisionTemplate {
                    owner: P0,
                    decisions: vec![PinnedDecision::Targets {
                        slot: DecisionSlot {
                            source: source.clone(),
                            index: 0,
                        },
                        targets: pins,
                    }],
                    replay: ReplayMode::Scheduled {
                        count: IterationCount::Fixed(3),
                    },
                    key: DecisionGroupKey::from_sources(
                        std::slice::from_ref(&source),
                        DecisionKind::LoopChoice,
                    ),
                }),
                per_cycle: None,
            },
        };
        state
    };
    let projected = |state: &GameState, viewer: PlayerId| -> Option<DecisionTemplate> {
        match engine::game::visibility::filter_state_for_viewer(state, viewer).waiting_for {
            WaitingFor::RespondToShortcut { proposal, .. } => proposal.template,
            other => panic!("the fixture parks on the CR 732.2b response window, got {other:?}"),
        }
    };

    // ── the hostile arm: one hidden hand card + one public seat in the SAME pin vector ──
    let hidden_state = window(vec![
        TargetPin::ByIdentity(YieldTarget::ThisObject {
            source_id: hidden_hand,
            incarnation: None,
            trigger_description: None,
        }),
        TargetPin::Player(P1),
    ]);
    assert!(
        projected(&hidden_state, P0).is_some(),
        "reach-guard + positive: the PROPOSER's own projection keeps the template, so the drops \
         below are keyed to the viewer boundary rather than to the fixture"
    );
    assert!(
        projected(&hidden_state, P1).is_none(),
        "CR 732.2b: the CURRENT responder must not receive a proposal whose pin names a card in \
         a hand they cannot see — and all-or-nothing means the public seat pin goes with it"
    );
    assert!(
        projected(&hidden_state, P2).is_none(),
        "the QUEUED responder is projected the same way: the guard keys on `proposal.proposer`, \
         not on the prompted `player`"
    );
    assert!(
        projected(&hidden_state, P3).is_some(),
        "positive: the OWNER of the pinned hand card keeps the template even though they are not \
         the proposer — the drop is keyed to what this viewer may actually see, so a redactor \
         that dropped the template for every non-proposer fails here"
    );

    // ── the paired positive: an all-seat template carries no hidden identity ──
    let public_state = window(vec![TargetPin::Player(P1)]);
    assert_eq!(
        projected(&public_state, P1),
        projected(&public_state, P0),
        "an all-seat template reaches the responder UNCHANGED — without this arm a redactor that \
         dropped every template would pass the negatives above"
    );
    assert!(
        projected(&public_state, P1).is_some(),
        "and it is genuinely present, not two matching `None`s"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R3-c — the `DecisionTemplate` carrier census over `WaitingFor`
// ─────────────────────────────────────────────────────────────────────────────────────────

/// HOW a `WaitingFor` variant reaches a `DecisionTemplate`: in its own body, or through a
/// field type that carries one. The intermediate type is NAMED rather than flattened to a
/// bool — "via which type" is a real axis (`RespondToShortcut` reaches the template through
/// `ShortcutProposal`) and a bool would erase it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CarrierKind {
    Direct,
    Via(String),
}

use syn::{GenericArgument, Item, PathArguments, Type};

/// Every type identifier `ty` names, outermost first: `Option<Vec<Foo>>` ⇒
/// `["Option", "Vec", "Foo"]`.
///
/// A NAME list rather than a `contains(marker)` over rendered text: the latter answers yes for
/// `DecisionTemplateAudit` and for the word inside a doc comment, and both would be carriers
/// this census invented.
fn type_names(ty: &Type) -> Vec<String> {
    fn walk(ty: &Type, out: &mut Vec<String>) {
        match ty {
            Type::Path(p) => {
                for seg in &p.path.segments {
                    out.push(seg.ident.to_string());
                    if let PathArguments::AngleBracketed(args) = &seg.arguments {
                        for arg in &args.args {
                            if let GenericArgument::Type(inner) = arg {
                                walk(inner, out);
                            }
                        }
                    }
                }
            }
            Type::Reference(r) => walk(&r.elem, out),
            Type::Slice(s) => walk(&s.elem, out),
            Type::Array(a) => walk(&a.elem, out),
            Type::Group(g) => walk(&g.elem, out),
            Type::Paren(p) => walk(&p.elem, out),
            Type::Tuple(t) => t.elems.iter().for_each(|e| walk(e, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ty, &mut out);
    out
}

/// Every item in `items`, INCLUDING the ones nested in inline `mod` blocks — a holder does not
/// stop being a holder for living inside a module.
fn flatten<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
    for item in items {
        out.push(item);
        if let Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                flatten(inner, out);
            }
        }
    }
}

/// `(variant name, its field types)` for `enum_name`, in declaration order.
fn enum_variants(src: &str, enum_name: &str) -> Vec<(String, Vec<Type>)> {
    let parsed =
        syn::parse_file(src).unwrap_or_else(|e| panic!("parse the `{enum_name}` source: {e}"));
    let mut items = Vec::new();
    flatten(&parsed.items, &mut items);
    items
        .iter()
        .find_map(|item| match item {
            Item::Enum(e) if e.ident == enum_name => Some(
                e.variants
                    .iter()
                    .map(|v| {
                        (
                            v.ident.to_string(),
                            v.fields.iter().map(|f| f.ty.clone()).collect(),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{enum_name}` must be declared in the parsed source"))
}

/// Every type declaration in the walked corpus whose own body names `marker` — struct, enum or
/// alias, at any visibility, generic or not, nested in a `mod` or not. THE DEPTH-1 STEP, and
/// the whole of it — see the row's disclosed limitation.
///
/// # Why `syn` and not string ops
///
/// This step used to scan for `pub struct <Ident> {` at column 0, which is a small fraction of
/// the declarations that can hold a `marker`. MEASURED over this walk root: that shape matches
/// **491** declarations, while **654** `pub enum`, **108** `pub(crate) struct` and **12**
/// generic `pub struct` heads are invisible to it — and a carrier the depth-1 step cannot see
/// is scored as a clean GREEN, which is the one failure mode a census must not have. `syn` is
/// already a `[dev-dependencies]` entry of this crate and already the instrument
/// `deterministic_game_state_serde` parses production sources with, so this is reuse and not a
/// new dependency. `PLANT 5` below is the arm that holds the four recovered forms.
///
/// Computed ONCE per corpus rather than re-scanned per candidate identifier: the walk is 500+
/// files and the enum is 128 variants, so the per-identifier form is quadratic in the corpus
/// for no extra signal.
fn types_carrying(corpus: &[(String, String)], marker: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for (file, src) in corpus {
        // A SOUND prefilter, not a shortcut: a declaration can only NAME `marker` if its file
        // text contains `marker`, so this skips ~500 parses without narrowing the answer.
        if !src.contains(marker) {
            continue;
        }
        let parsed = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {file}: {e}"));
        let mut items = Vec::new();
        flatten(&parsed.items, &mut items);
        for item in items {
            let (name, field_types): (String, Vec<&Type>) = match item {
                Item::Struct(s) => (
                    s.ident.to_string(),
                    s.fields.iter().map(|f| &f.ty).collect(),
                ),
                Item::Enum(e) => (
                    e.ident.to_string(),
                    e.variants
                        .iter()
                        .flat_map(|v| v.fields.iter().map(|f| &f.ty))
                        .collect(),
                ),
                Item::Type(t) => (t.ident.to_string(), vec![t.ty.as_ref()]),
                _ => continue,
            };
            if field_types
                .iter()
                .flat_map(|t| type_names(t))
                .any(|n| n == marker)
            {
                out.insert(name);
            }
        }
    }
    out
}

/// Every variant of `enum_name` in `enum_src` that reaches `marker`, DIRECTLY or through one
/// field type found in `corpus`.
///
/// SOURCE-PARAMETERIZED on purpose: every probe below plants into an in-memory `String`, so the
/// census's own discrimination costs no compile and mutates no worktree file. Mirrors
/// `super::loop_shortcut_offer_writer_census::classify`'s `(src, needle, file)` shape.
fn carriers_in_source(
    enum_src: &str,
    enum_name: &str,
    corpus: &[(String, String)],
    marker: &str,
    walk_via: bool,
) -> Vec<(String, CarrierKind)> {
    let via_types = types_carrying(corpus, marker);
    let mut out = Vec::new();
    for (name, field_types) in enum_variants(enum_src, enum_name) {
        // Outermost-first across the variant's fields in declaration order, so `Via` names the
        // holder a reader would name.
        let named: Vec<String> = field_types.iter().flat_map(type_names).collect();
        if named.iter().any(|n| n == marker) {
            out.push((name, CarrierKind::Direct));
            continue;
        }
        if !walk_via {
            continue;
        }
        if let Some(via) = named.into_iter().find(|n| via_types.contains(n)) {
            out.push((name, CarrierKind::Via(via)));
        }
    }
    out
}

/// Does `filter_state_for_viewer`'s body carry an `if let WaitingFor::<name>` dispatch arm?
fn redaction_arm_present(visibility_src: &str, name: &str) -> bool {
    let needle = format!("if let {}::{name}", "WaitingFor");
    let mut inside = false;
    for line in visibility_src.lines() {
        if !inside {
            inside = line.starts_with("pub fn filter_state_for_viewer(");
            continue;
        }
        if line == "}" {
            break;
        }
        if line.contains(&needle) {
            return true;
        }
    }
    false
}

/// Splice `injected` in immediately above `enum_name`'s closing brace, on a COPY of `src`.
fn plant_into_enum(src: &str, enum_name: &str, injected: &str) -> String {
    let open = format!("pub enum {enum_name} {{");
    let mut out = String::new();
    let mut inside = false;
    let mut planted = false;
    for line in src.lines() {
        if !planted {
            if inside && line == "}" {
                out.push_str(injected);
                planted = true;
            } else if !inside && line.trim_start().starts_with(&open) {
                inside = true;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(
        planted,
        "the plant anchor `{open}` … `}}` must exist in the copy"
    );
    out
}

/// **R3-c** — exactly TWO `WaitingFor` variants carry a `DecisionTemplate`, and both have a
/// redaction arm inside `filter_state_for_viewer`.
///
/// MEASURED at this tip: `{(LoopShortcut, Direct), (RespondToShortcut, Via(ShortcutProposal))}`
/// out of 128 variants; the VIA target is `analysis::loop_check::ShortcutProposal`'s
/// `pub template: Option<DecisionTemplate>`.
///
/// # Why a census exists here at all
///
/// The REDACTION DISPATCH in `filter_state_for_viewer` is two `if let`s, not a `match`, so a
/// THIRD carrier gets no compile error THERE — that is the gap this row closes. It is not true
/// elsewhere: **at least 9** exhaustive `match`es on `WaitingFor` would fail E0004, spread over
/// two crates (`engine`, `phase-ai`) — measured by whole-workspace AST enumeration over
/// `crates/`, which is a **lower bound**, not a total. Those matches make a new variant hard to
/// ADD; not one of them makes it hard to add UNREDACTED. **The site list is deliberately not
/// enumerated here** — a frozen list in a doc comment is a claim no test defends, and it rots
/// the moment a crate is added.
///
/// # Why the probes plant into a `String` instead of adding a variant
///
/// MEASURED: a real third variant yields 6 E0004 under `cargo check -p phase-engine --lib` and
/// 7 with `--features test-support`, and BOTH runs abort at the lib — so the dependent crates
/// and this ~4 800-row integration target are never type-checked, and the repair list is
/// neither stable nor bounded. Source injection has neither problem and needs no build.
///
/// # Discrimination — five plant arms, all RUN, all over in-memory copies
///
/// * a 3rd DIRECT and a 4th VIA carrier planted into a copy of the enum source ⇒ the set
///   assertion fails NAMING both new variants (`n = 4`);
/// * the depth-1 VIA step disabled ⇒ the set shrinks to `{LoopShortcut}` ⇒ fails, so the
///   transitive step is load-bearing rather than decoration;
/// * a synthetic enum with no carrier at all ⇒ `n = 0`, so the classifier cannot only ever
///   return the answer this row wants;
/// * the `RespondToShortcut` arm deleted from a copy of `visibility.rs` ⇒ the redaction half
///   flips to `false` for that carrier while `LoopShortcut` stays `true`. The mutation is on a
///   COPY, so the shipped row `respond_to_shortcut_template_redacts_a_hidden_pin_for_non_proposers`
///   above is not perturbed;
/// * a `pub(crate)`, a GENERIC, an ENUM, an ALIAS and a `mod`-NESTED holder planted at once ⇒
///   all five are NAMED as `Via` carriers, and a non-carrier planted beside them is not. These
///   are the forms the retired `pub struct <Ident> {{` string scan could not see, and each was
///   a false GREEN rather than a false alarm.
///
/// # DISCLOSED LIMITATIONS
///
/// 1. **The walk is DEPTH-1.** A `WaitingFor` field whose type reaches a `DecisionTemplate` two
///    levels down is invisible to it. Measured today: zero such types exist. That is a latent
///    gap, not a covered case.
/// 2. **The E0004 figure above is a LOWER BOUND from a named instrument**, not a total, and this
///    row must never be "helpfully" upgraded into a site list.
#[test]
fn exactly_two_waiting_for_variants_carry_a_decision_template_and_both_are_redacted() {
    use super::loop_shortcut_offer_writer_census::rs_files;

    // Assembled at runtime for the same reason both sibling censuses assemble their anchors:
    // an instrument that can count its own needle after a future move is one that lies about
    // the surface it measures.
    let marker = format!("{}{}", "Decision", "Template");
    let engine_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let corpus: Vec<(String, String)> = rs_files(&engine_src)
        .into_iter()
        .map(|path| {
            let src =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            let rel = path
                .strip_prefix(&engine_src)
                .expect("walked path is under its root")
                .to_string_lossy()
                .replace('\\', "/");
            (rel, src)
        })
        .collect();
    let file_of = |suffix: &str| -> String {
        corpus
            .iter()
            .find(|(f, _)| f.ends_with(suffix))
            .unwrap_or_else(|| panic!("the walk must reach {suffix}"))
            .1
            .clone()
    };
    let enum_src = file_of("types/game_state.rs");
    let visibility_src = file_of("game/visibility.rs");

    // ── the classifier's own reach-guard: the enum was actually found ──
    let total = enum_variants(&enum_src, "WaitingFor").len();
    assert_eq!(
        total, 130,
        "`WaitingFor` has 130 variants at this tip, read off the `syn` parse. This number is \
         pinned so a variant REMOVED is as visible as one added; if you added a variant and it \
         carries no `DecisionTemplate`, update this number. A wildly different count means the \
         reader lost its anchor, and every assertion below would then be measuring an empty enum"
    );
    // 128 ⇒ 129 is ADJUDICATED, not bumped: upstream #7336 ("make dig entries attack") added
    // `EntryAttackTargetChoice { player, object_id, valid_targets }`. Measured, because the
    // number alone cannot say it: that variant carries NO `DecisionTemplate` (zero matches in
    // its body), so it is not a third carrier and the assertion below is unchanged by it. The
    // count moved for a reason that does not touch this row's subject — which is exactly the
    // case this reach-guard exists to make visible rather than silent.
    // 129 ⇒ 130 is ADJUDICATED on the same terms: upstream #7382 ("choose pre-entry opponent
    // controller") added `EntryControllerChoice { player: PlayerId, candidates: Vec<PlayerId> }`
    // (CR 614.12a). Measured, not inferred from the diff: that body holds NO `DecisionTemplate`,
    // so it is not a third carrier, and both the carrier vec and the redaction loop below are
    // unchanged by it. The count itself was read from THIS assertion's own failure (`left: 130`)
    // rather than from a hand-written variant counter — one was tried and returned 49 while
    // contradicting itself, and a second instrument that disagrees with the `syn` parse is worth
    // less than no second instrument. The reach-guard did its whole job here: this drift produced
    // no merge conflict and could not have, so CI was the only thing between it and shipping.

    let carriers = carriers_in_source(&enum_src, "WaitingFor", &corpus, &marker, true);
    assert_eq!(
        carriers,
        vec![
            ("LoopShortcut".to_string(), CarrierKind::Direct),
            (
                "RespondToShortcut".to_string(),
                CarrierKind::Via("ShortcutProposal".to_string())
            ),
        ],
        "CR 732.2a / CR 723.4: exactly TWO `WaitingFor` variants carry a `DecisionTemplate` — \
         `LoopShortcut` directly and `RespondToShortcut` through `ShortcutProposal` — and both \
         are named rather than counted, because a census asserting `>= 1 carrier` is vacuous. A \
         THIRD carrier is a new per-viewer redaction obligation: the dispatch in \
         `filter_state_for_viewer` is `if let`s, so nothing will fail to compile. got {carriers:?}"
    );

    // ── the redaction half, read from the production source ──
    for (name, kind) in &carriers {
        assert!(
            redaction_arm_present(&visibility_src, name),
            "CR 723.4: every `DecisionTemplate` carrier needs its own dispatch arm inside \
             `filter_state_for_viewer`; `{name}` ({kind:?}) has none"
        );
    }

    // ── PLANT 1 — a 3rd DIRECT and a 4th VIA carrier, into COPIES ──
    let planted_src = plant_into_enum(
        &enum_src,
        "WaitingFor",
        &format!(
            "    ProbeThirdCarrier {{\n        template: Option<{marker}>,\n    }},\n\
             \x20   ProbeFourthCarrier {{\n        holder: ProbeViaHolder,\n    }},\n"
        ),
    );
    let mut planted_corpus = corpus.clone();
    planted_corpus.push((
        "planted.rs".to_string(),
        format!("pub struct ProbeViaHolder {{\n    pub template: Option<{marker}>,\n}}\n"),
    ));
    let planted = carriers_in_source(&planted_src, "WaitingFor", &planted_corpus, &marker, true);
    assert_eq!(
        planted
            .iter()
            .map(|(n, k)| (n.as_str(), k.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("LoopShortcut", CarrierKind::Direct),
            (
                "RespondToShortcut",
                CarrierKind::Via("ShortcutProposal".to_string())
            ),
            ("ProbeThirdCarrier", CarrierKind::Direct),
            (
                "ProbeFourthCarrier",
                CarrierKind::Via("ProbeViaHolder".to_string())
            ),
        ],
        "ANTI-VACUITY: the census must NAME a planted third (direct) and fourth (transitive) \
         carrier, else the two-carrier answer above is what a dead instrument returns. \
         got {planted:?}"
    );

    // ── PLANT 2 — the depth-1 VIA step disabled ──
    let no_via = carriers_in_source(&enum_src, "WaitingFor", &corpus, &marker, false);
    assert_eq!(
        no_via,
        vec![("LoopShortcut".to_string(), CarrierKind::Direct)],
        "the transitive step is LOAD-BEARING: without it `RespondToShortcut` is invisible and \
         the shipped set assertion above would be a one-element claim. got {no_via:?}"
    );

    // ── PLANT 3 — a synthetic enum with no carrier ──
    let synthetic = "pub enum WaitingFor {\n    Alpha {\n        player: PlayerId,\n    },\n    \
                     Beta {\n        x: u32,\n    },\n}\n";
    let none = carriers_in_source(synthetic, "WaitingFor", &corpus, &marker, true);
    assert!(
        none.is_empty(),
        "a carrier-free enum must classify as carrier-free; an instrument that can only ever \
         return {{LoopShortcut, RespondToShortcut}} is what this arm forecloses. got {none:?}"
    );

    // ── PLANT 4 — the redaction arm deleted, on a COPY of `visibility.rs` ──
    let mutated_visibility = visibility_src.replace(
        &format!("if let {}::RespondToShortcut", "WaitingFor"),
        &format!("if let {}::ZzzDeletedArm", "WaitingFor"),
    );
    assert!(
        redaction_arm_present(&mutated_visibility, "LoopShortcut")
            && !redaction_arm_present(&mutated_visibility, "RespondToShortcut"),
        "the redaction half must FLIP when its arm is deleted from the copy, and only for the \
         carrier whose arm was deleted — otherwise the `for` loop above asserts nothing"
    );

    // ── PLANT 5 — the four holder FORMS the retired string scan could not see, plus a
    //    non-carrier control. Each is a real declaration shape from this walk root: over
    //    `crates/engine/src` the old `pub struct <Ident> {` shape matched 491 declarations
    //    while 654 `pub enum`, 108 `pub(crate) struct` and 12 generic `pub struct` heads were
    //    invisible — every one of them a carrier that would have scored a clean GREEN. ──
    let forms = format!(
        "pub(crate) struct CrateVisHolder {{\n    pub template: Option<{marker}>,\n}}\n\
         pub struct GenericHolder<'a, T> {{\n    pub template: &'a {marker},\n    pub t: T,\n}}\n\
         pub enum EnumHolder {{\n    WithTemplate({marker}),\n    Without,\n}}\n\
         pub type AliasHolder = Option<{marker}>;\n\
         pub struct NotAHolder {{\n    pub seat: PlayerId,\n}}\n\
         mod inner {{\n    pub struct NestedHolder {{\n        pub template: \
         Option<super::{marker}>,\n    }}\n}}\n"
    );
    let mut forms_corpus = corpus.clone();
    forms_corpus.push(("forms.rs".to_string(), forms));
    let forms_src = plant_into_enum(
        &enum_src,
        "WaitingFor",
        "    ProbeCrateVis {\n        h: CrateVisHolder,\n    },\n\
         \x20   ProbeGeneric {\n        h: GenericHolder<'static, u8>,\n    },\n\
         \x20   ProbeEnumHolder {\n        h: EnumHolder,\n    },\n\
         \x20   ProbeAlias {\n        h: AliasHolder,\n    },\n\
         \x20   ProbeNested {\n        h: NestedHolder,\n    },\n\
         \x20   ProbeNonCarrier {\n        h: NotAHolder,\n    },\n",
    );
    let by_forms: Vec<(String, CarrierKind)> =
        carriers_in_source(&forms_src, "WaitingFor", &forms_corpus, &marker, true)
            .into_iter()
            .filter(|(n, _)| n.starts_with("Probe"))
            .collect();
    assert_eq!(
        by_forms,
        vec![
            (
                "ProbeCrateVis".to_string(),
                CarrierKind::Via("CrateVisHolder".to_string())
            ),
            (
                "ProbeGeneric".to_string(),
                CarrierKind::Via("GenericHolder".to_string())
            ),
            (
                "ProbeEnumHolder".to_string(),
                CarrierKind::Via("EnumHolder".to_string())
            ),
            (
                "ProbeAlias".to_string(),
                CarrierKind::Via("AliasHolder".to_string())
            ),
            (
                "ProbeNested".to_string(),
                CarrierKind::Via("NestedHolder".to_string())
            ),
        ],
        "the depth-1 step must see a `pub(crate)`, a GENERIC, an ENUM, an ALIAS and a \
         `mod`-NESTED holder alike — and `ProbeNonCarrier` must be ABSENT, because an \
         instrument that reports every variant is as useless as one that reports too few. \
         got {by_forms:?}"
    );
}

/// F4 (review finding): the THIRD carrier of the same `Vec<PinnedDecision>` —
/// `GameState::last_loop_action_sequence[].pins` — routes through the same authority.
///
/// It is serialized whenever non-empty (`skip_serializing_if = "Vec::is_empty"`, not `skip`) and
/// had zero hits in `visibility.rs` before this change. Its three production writers (the
/// `record_loop_pin` call sites: a mana-ability tap cost, a mana-color choice, a proliferate
/// target) can only name battlefield permanents and seats, so no board the engine mints today
/// reaches the redaction — this row constructs the pin a fourth writer would produce, which is
/// the only way to hold the seam closed before that writer exists.
///
/// # Non-vacuity
///
/// The owner arm (P1 sees their own hand card) is the paired positive: a sweep that cleared every
/// recorded pin would satisfy the negative and fail it. The step itself is asserted to survive in
/// both arms, so "the whole sequence was dropped" cannot masquerade as a pass.
///
/// REVERT-PROBE: delete the `for step in &mut filtered.last_loop_action_sequence` sweep ⇒ P2 keeps
/// the hidden-hand pin ⇒ the `is_empty()` assertion FAILS while both positives stay green.
///
/// *What wrong implementation would still pass this row?* One that clears `pins` unconditionally
/// for every non-owner — the owner arm is what rejects it.
#[test]
fn recorded_loop_pins_are_redacted_for_a_viewer_who_cannot_see_the_pinned_object() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hidden_hand = scenario.add_bolt_to_hand(P1); // a hidden card in P1's hand
    let runner = scenario.build();

    let mut state = runner.state().clone();
    let card_id = state.objects[&hidden_hand].card_id;
    state.last_loop_action_sequence = vec![LoopActionContext {
        card_id,
        controller: P1,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::NotUsed,
        },
        convoke: None,
        pins: vec![PinnedDecision::Targets {
            slot: DecisionSlot {
                source: YieldTarget::ThisObject {
                    source_id: ObjectId(999),
                    incarnation: None,
                    trigger_description: None,
                },
                index: 0,
            },
            targets: vec![
                TargetPin::ByIdentity(YieldTarget::ThisObject {
                    source_id: hidden_hand,
                    incarnation: None,
                    trigger_description: None,
                }),
                TargetPin::Player(P1),
            ],
        }],
    }];

    let pins_for = |viewer: PlayerId| -> Vec<PinnedDecision> {
        let seq = engine::game::visibility::filter_state_for_viewer(&state, viewer)
            .last_loop_action_sequence;
        assert_eq!(
            seq.len(),
            1,
            "the recorded step itself is never dropped — only its pin vector is redacted"
        );
        seq[0].pins.clone()
    };

    assert_eq!(
        pins_for(P1).len(),
        1,
        "positive: the hand's OWNER keeps the recorded pin — `target_hidden` answers false for a \
         card this viewer may privately see"
    );
    assert!(
        pins_for(P2).is_empty(),
        "a viewer who cannot see P1's hand receives no pin naming that card — all-or-nothing, so \
         the public seat pin in the same vector goes with it"
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
/// guard (in `game/engine.rs`). The latched PREDICTED WINNER (not the proposer) concedes DURING the
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
/// (`game/ability_scan.rs`'s `ability_definition_axes` destructures it as `_`), so a row keyed on that field would
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
    // Decode AS `PersistedGameState` rather than decoding a bare `GameState` and wrapping
    // it in `Raw`: only the former runs `reject_legacy_raw_prompt_authority` and
    // `decode_persisted_resolution_state`, which is the rest of the production chokepoint
    // — including the CR 732.2a load-seam bound invariant `w15_*` below pins.
    // `.expect(..)`, not `?`: `into_game_state` returns `GameState`, not `Result`.
    serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        envelope["gameState"].clone(),
    )
    .expect("gameState deserializes through the production decoder")
    .into_game_state()
}

/// The migrated dellian dump's `gameState`, as a raw `serde_json::Value`.
fn dellian_game_state_value() -> serde_json::Value {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    envelope["gameState"].clone()
}

/// R0c — the migrated fixture decodes through BOTH decoders, and an un-migrated one
/// through NEITHER.
///
/// This is the row that converts the next upstream save-compat break into a named
/// regression instead of four unrelated-looking red rows. Upstream #6718 (`0468df1f4`)
/// added `TargetSelectionSlot::effect_kind` with no `#[serde(default)]`; every dump
/// fixture captured before it became undecodable, and nothing said so in one place.
///
/// The negative arm IS the positive arm's anti-vacuity control: without it, an
/// `assert!(ok)` pair would pass on any value at all, including one where the field was
/// never consulted. Both arms operate on the SAME value, differing only by the presence
/// of `effect_kind`, so the verdict is attributable to that field and nothing else.
#[test]
fn migrated_dump_decodes_through_both_decoders_and_unmigrated_through_neither() {
    let migrated = dellian_game_state_value();

    // Reach-guard: prove the mutation below has something to remove. A value with no
    // `effect_kind` key would make the negative arm's `Err` unattributable.
    let slots = migrated["waiting_for"]["data"]["target_slots"]
        .as_array()
        .expect("the dellian dump publishes a target_slots array");
    assert_eq!(
        slots.len(),
        1,
        "the dellian dump publishes exactly one slot"
    );
    assert!(
        slots[0].get("effect_kind").is_some(),
        "the MIGRATED fixture must carry effect_kind — if this fails, the migration script \
         was not run, and the negative arm below would pass for the wrong reason"
    );

    // POSITIVE: both the direct `GameState` decode and the production `PersistedGameState`
    // decode accept the migrated value.
    assert!(
        serde_json::from_value::<GameState>(migrated.clone()).is_ok(),
        "migrated fixture must decode as a bare GameState"
    );
    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(migrated.clone())
            .is_ok(),
        "migrated fixture must decode through the PRODUCTION decoder"
    );

    // R8 — THE ROUTING CANONICALIZES PERSISTED PROVENANCE. The six loaders stopped decoding
    // a bare `GameState` and wrapping it in `PersistedGameState::Raw`, and started decoding AS
    // `PersistedGameState`: a different type, a different `Deserialize`, and a different
    // conversion (`decode_persisted_resolution_state`, which injects
    // `resolution_state_version` and decodes the resolution state as `ResolutionStateWire`).
    // The persistence boundary assigns the current-turn occurrence namespace to the old
    // ledger and matching live stack events; direct-current raw decode deliberately does not.
    // Every other serialized surface must remain unchanged.
    //
    // `GameState` has no `PartialEq`, so compare its serialized forms recursively.
    // Hash-backed owners now serialize canonically at their field boundaries; arrays are
    // therefore always order-sensitive here, including libraries, stacks, and trigger order.
    // Sorting below is diagnostic only: it distinguishes reordering from changed membership
    // without ever accepting either difference.

    /// Collect every path at which `a` and `b` differ in a way that set-ordering cannot
    /// explain. Returns an empty vec iff the two states are equal modulo set order.
    fn differences(
        a: &serde_json::Value,
        b: &serde_json::Value,
        path: &str,
        out: &mut Vec<String>,
    ) {
        use serde_json::Value;
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    match (x.get(key), y.get(key)) {
                        (Some(l), Some(r)) => differences(l, r, &format!("{path}.{key}"), out),
                        _ => out.push(format!("{path}.{key} (present on one side only)")),
                    }
                }
            }
            (Value::Array(x), Value::Array(y)) if x == y => {}
            (Value::Array(x), Value::Array(y)) => {
                let (mut xs, mut ys) = (x.clone(), y.clone());
                let key = |v: &Value| v.to_string();
                xs.sort_by_key(key);
                ys.sort_by_key(key);
                if xs == ys {
                    out.push(format!("{path} (REORDERED)"));
                } else {
                    out.push(format!("{path} (different elements)"));
                }
            }
            _ if a == b => {}
            _ => out.push(path.to_string()),
        }
    }

    let serialized = |state: &GameState| serde_json::to_value(state).expect("GameState serializes");
    let legacy_restored = {
        let raw: GameState = serde_json::from_value(migrated.clone())
            .expect("the pre-routing loader form: a bare GameState decode");
        engine::types::game_state::PersistedGameState::Raw(Box::new(raw)).into_game_state()
    };
    let routed_restored =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(migrated.clone())
            .expect("the routed loader form: decode AS PersistedGameState")
            .into_game_state();
    let routed_value = serialized(&routed_restored);
    let mut diffs = Vec::new();
    differences(
        &serialized(&legacy_restored),
        &routed_value,
        "state",
        &mut diffs,
    );
    assert_eq!(
        diffs,
        vec![
            "state.stack (different elements)".to_string(),
            "state.zone_changes_this_turn (different elements)".to_string(),
        ],
        "the production restore changes only the persisted zone-change provenance surfaces"
    );

    let stack_zone_change_keys = |state: &GameState| {
        state
            .stack
            .iter()
            .filter_map(|entry| {
                let StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::ZoneChanged { record, .. }),
                    ..
                } = &entry.kind
                else {
                    return None;
                };
                Some((record.recorded_turn_number, record.turn_zone_change_index))
            })
            .collect::<Vec<_>>()
    };
    let legacy_stack_keys = stack_zone_change_keys(&legacy_restored);
    let routed_stack_keys = stack_zone_change_keys(&routed_restored);
    assert!(
        !legacy_stack_keys.is_empty(),
        "reach-guard: the fixture has live stack ZoneChanged trigger contexts"
    );
    assert!(
        legacy_restored
            .zone_changes_this_turn
            .iter()
            .all(|record| record.recorded_turn_number == 0)
            && legacy_stack_keys.iter().all(|(turn, _)| *turn == 0),
        "direct-current raw decode retains the historical zero provenance defaults"
    );
    assert_eq!(
        routed_restored
            .zone_changes_this_turn
            .iter()
            .enumerate()
            .map(|(index, record)| (
                record.recorded_turn_number,
                record.turn_zone_change_index,
                index
            ))
            .collect::<Vec<_>>(),
        (0..routed_restored.zone_changes_this_turn.len())
            .map(|index| (routed_restored.turn_number, index, index))
            .collect::<Vec<_>>(),
        "production restore stamps each retained ledger row in its current-turn namespace"
    );
    assert!(
        routed_stack_keys
            .iter()
            .all(|(turn, _)| *turn == routed_restored.turn_number),
        "production restore stamps every matching live stack event with the current turn"
    );
    assert_eq!(
        routed_stack_keys
            .iter()
            .map(|(_, index)| *index)
            .collect::<Vec<_>>(),
        legacy_stack_keys
            .iter()
            .map(|(_, index)| *index)
            .collect::<Vec<_>>(),
        "production restore preserves each stack event's ledger occurrence identity"
    );

    // The perturbation IS that assertion's reach-guard: without it, a comparison that
    // compared a value to itself — or explained every difference away as set order — would
    // pass on any two states at all. Perturb ONE scalar; the comparison must SEE it, and
    // must name the field it saw.
    let mut perturbed = legacy_restored.clone();
    perturbed.turn_number += 1;
    let mut perturbed_diffs = Vec::new();
    differences(
        &serialized(&perturbed),
        &serialized(&legacy_restored),
        "state",
        &mut perturbed_diffs,
    );
    assert_eq!(
        perturbed_diffs,
        vec!["state.turn_number".to_string()],
        "the comparison must see a one-scalar difference AND name it; if it cannot, the \
         canonicalization-surface comparison above proves nothing"
    );

    // NEGATIVE (the anti-vacuity control): strip the field back out and both must reject.
    let mut unmigrated = migrated;
    unmigrated["waiting_for"]["data"]["target_slots"]
        .as_array_mut()
        .expect("target_slots is an array")
        .iter_mut()
        .for_each(|slot| {
            slot.as_object_mut()
                .expect("each slot is an object")
                .remove("effect_kind")
                .expect("each slot carried effect_kind before removal");
        });
    assert!(
        serde_json::from_value::<GameState>(unmigrated.clone()).is_err(),
        "an un-migrated save must NOT decode as a bare GameState"
    );
    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(unmigrated)
            .is_err(),
        "an un-migrated save must NOT decode through the production decoder — the strict \
         decoder is the point; a #[serde(default)] shim would silently accept it"
    );
}

/// R0d — CR 732.2a: a persisted `LoopShortcut` offer whose WIRE bound is `0` must fail the
/// load, and one whose bound is `5` must not.
///
/// The defect this pins (W15) is real and was measured before the fix: a wire
/// `max_iterations: 0` deserialized clean, satisfied `is_bounded()`, and reached
/// `ai_support/candidates.rs`, which echoed it as a declared `IterationCount::Fixed(0)` —
/// so the engine opened the CR 732.2b response window for an offer that admits no legally
/// takeable sequence. The offer was corrupt one beat BEFORE any count was declared.
///
/// ⚠ THE FIXTURE CHOICE IS LOAD-BEARING — this row uses TENACITY, not the dellian dump the
/// dual-decode row uses. The dellian value is `TriggerTargetSelection` and carries no
/// `schema` object at all, so `…schema.max_iterations` cannot even be written onto it: both
/// arms would decode identically, for a reason having nothing to do with the invariant.
/// The tenacity dump is the only in-tree `LoopShortcut` capture.
///
/// The key is ABSENT in the fixture, so the mutation CREATES it — asserted below, because a
/// mutation that silently failed to apply would make the `0` arm's verdict meaningless.
///
/// ⚠ NON-VACUITY DEPENDS ON THE ROUTED LOADER. This row decodes AS `PersistedGameState`;
/// the pre-5c loader form (`PersistedGameState::Raw(Box::new(bare_decode))`) bypasses
/// `PersistedGameState`'s own `Deserialize` and therefore never runs
/// `decode_persisted_resolution_state` at all, so the same assertions written against that
/// form could not fail. REVERT-PROBE: delete `reject_zero_bound_shortcut_offer`'s body (or
/// its call) ⇒ the `0` arm decodes `Ok` ⇒ FAILS. The wire-`5` arm is the anti-vacuity half:
/// it proves the mutation instrument reaches the field and that the invariant refuses `0`
/// specifically rather than refusing every mutated save.
/// The SIBLING wire zero on the same offer: `PeriodicDelta::frames_per_period`.
///
/// `max_iterations` says how many repetitions a proposal commits; `frames_per_period` says what
/// ONE repetition is. `drive_one_shortcut_cycle` closes a cycle on
/// `frames_per_period.is_some_and(|k| frames_this_cycle >= k)`, and `frames_this_cycle` is a
/// `u32` — so `k == 0` makes that disjunct a TAUTOLOGY, ending every "cycle" at the first
/// active-player priority beat instead of at the certified CR 732.2a period.
///
/// THE PERIOD IS BUILT IN RUST AND SERIALIZED, not hand-written as JSON. `ResourceVector`'s
/// per-player maps are keyed by `PlayerId` and need the wire adapter to cross the boundary, so a
/// hand-authored object risks testing a shape production never emits — the failure mode this
/// suite has hit before.
///
/// REVERT-PROBE: delete the `frames_per_period == 0` block in `reject_zero_bound_shortcut_offer`
/// ⇒ the `0` arm decodes `Ok` ⇒ this row FAILS while
/// `a_wire_zero_shortcut_bound_fails_the_load_and_a_wire_five_does_not` stays green, because the
/// fixture omits `max_iterations` entirely and defaults it to `MAX_SHORTCUT_CYCLES`. The `2` arm
/// is the anti-vacuity half: it proves the splice reaches the field and that the guard refuses
/// `0` specifically rather than refusing every save carrying a period.
#[test]
fn a_wire_zero_frames_per_period_fails_the_load_and_a_wire_two_does_not() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let base = envelope["gameState"].clone();

    assert_eq!(
        base["waiting_for"]["type"].as_str(),
        Some("LoopShortcut"),
        "the invariant is scoped to the one variant that carries a LoopCertificate"
    );
    assert!(
        base["waiting_for"]["data"]["certificate"]["per_cycle"].is_null(),
        "the fixture carries no certified period, so the splice below CREATES it"
    );

    let with_frames = |frames: u32| {
        let mut v = base.clone();
        let period = engine::analysis::resource::PeriodicDelta {
            frames_per_period: frames,
            delta: Default::default(),
            victim_slot: vec![],
        };
        v["waiting_for"]["data"]["certificate"]["per_cycle"] =
            serde_json::to_value(&period).expect("a PeriodicDelta serializes");
        assert_eq!(
            v["waiting_for"]["data"]["certificate"]["per_cycle"]["frames_per_period"].as_u64(),
            Some(u64::from(frames)),
            "the splice must reach certificate.per_cycle.frames_per_period"
        );
        v
    };

    let message =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(with_frames(0))
            .expect_err("a wire frames_per_period of 0 must fail the load")
            .to_string();
    assert!(
        message.contains("frames_per_period 0"),
        "the rejection must NAME the invariant it enforces, and must not be the sibling \
         max_iterations guard firing instead, got: {message}"
    );

    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(with_frames(2))
            .is_ok(),
        "a wire frames_per_period of 2 is a legal certified span and must still load"
    );

    // ── THE SECOND WIRE HOST, AND THE ONE THE DRIVE ACTUALLY READS ──────────────────────
    // `frames_per_period` is a `PeriodicDelta` field, not a `LoopCertificate` field, and
    // `ShortcutProposal` carries its own `per_cycle`. `materialize_fixed_shortcut` feeds the
    // drive from `proposal.per_cycle.as_ref().map(|pd| pd.frames_per_period)` — THIS host —
    // never from the offer's certificate. A restored `RespondToShortcut` is reached without
    // re-entering `LoopShortcut`, so guarding only the arms above would leave the consumed path
    // open while looking complete.
    //
    // REVERT-PROBE: delete the `RespondToShortcut` block in `reject_zero_bound_shortcut_offer`
    // ⇒ every arm above still passes and only this one flips to `Ok`.
    let respond_with_frames = |frames: u32| {
        let mut v = base.clone();
        let waiting = engine::types::game_state::WaitingFor::RespondToShortcut {
            player: engine::types::player::PlayerId(1),
            remaining_players: vec![],
            proposal: engine::analysis::loop_check::ShortcutProposal {
                proposer: engine::types::player::PlayerId(0),
                predicted_winner: None,
                count: engine::analysis::decision_template::IterationCount::Fixed(3),
                unbounded: vec![],
                win_kind: engine::analysis::loop_check::WinKind::Advantage,
                template: None,
                per_cycle: Some(engine::analysis::resource::PeriodicDelta {
                    frames_per_period: frames,
                    delta: Default::default(),
                    victim_slot: vec![],
                }),
            },
        };
        v["waiting_for"] = serde_json::to_value(&waiting).expect("a WaitingFor serializes");
        assert_eq!(
            v["waiting_for"]["data"]["proposal"]["per_cycle"]["frames_per_period"].as_u64(),
            Some(u64::from(frames)),
            "the splice must reach proposal.per_cycle.frames_per_period"
        );
        v
    };

    let message = serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        respond_with_frames(0),
    )
    .expect_err("a wire frames_per_period of 0 on the PROPOSAL must fail the load")
    .to_string();
    assert!(
        message.contains("frames_per_period 0") && message.contains("RespondToShortcut"),
        "the rejection must name BOTH the invariant and the host that carried it, so a reader \
         can tell which of the two per_cycle sites fired, got: {message}"
    );

    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(
            respond_with_frames(2)
        )
        .is_ok(),
        "a legal span on the proposal host must still load — anti-vacuity for the arm above"
    );
}

#[test]
fn a_wire_zero_shortcut_bound_fails_the_load_and_a_wire_five_does_not() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let base = envelope["gameState"].clone();

    // Reach-guards on the fixture itself.
    assert_eq!(
        base["waiting_for"]["type"].as_str(),
        Some("LoopShortcut"),
        "R0d must run on a LoopShortcut capture — the invariant is scoped to that variant"
    );
    assert!(
        base["waiting_for"]["data"]["schema"].is_object(),
        "the tenacity offer carries a schema object for the bound to live on"
    );
    assert!(
        base["waiting_for"]["data"]["schema"]
            .get("max_iterations")
            .is_none(),
        "the fixture predates the field, so the mutation below CREATES the key"
    );

    let with_bound = |n: u64| {
        let mut v = base.clone();
        v["waiting_for"]["data"]["schema"]["max_iterations"] = serde_json::json!(n);
        // Anti-vacuity: prove the write landed before drawing any conclusion from it.
        assert_eq!(
            v["waiting_for"]["data"]["schema"]["max_iterations"].as_u64(),
            Some(n),
            "the mutation must reach schema.max_iterations"
        );
        v
    };

    // CR 732.2a: a proposal must describe "a sequence of game choices ... that may be
    // legally taken based on the current game state". A bound of 0 admits none.
    let zero =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(with_bound(0));
    let message = zero
        .expect_err("a wire max_iterations of 0 must fail the load, not revive a corrupt offer")
        .to_string();
    assert!(
        message.contains("max_iterations 0"),
        "the rejection must NAME the invariant it enforces, got: {message}"
    );

    // The control: same fixture, same instrument, same mutated key, a legal bound.
    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(with_bound(5))
            .is_ok(),
        "a wire max_iterations of 5 is a legal bound and must still load"
    );

    // ── THE SECOND INGRESS ──────────────────────────────────────────────────────────
    // CR 732.2a again, through the OTHER decode entry point. Upstream #6933 split the
    // decode surface in two: `GameStateDecode::decode_persisted_resolution_state` (which
    // the arms above reach) deserializes `ResolutionStateWire` itself and NEVER calls
    // `GameStateDecode::decode`. A bare `GameState` decode routes through `decode` with
    // `GameStateDecodeMode::DirectCurrentRaw` instead (see `impl Deserialize for
    // GameState`), so hosting the bound guard on only one of them leaves this path able
    // to revive exactly the corrupt offer the arms above refuse.
    //
    // REVERT-PROBE: delete the `reject_zero_bound_shortcut_offer` call in
    // `GameStateDecode::decode` ONLY — leaving the one in
    // `decode_persisted_resolution_state` — and every arm above still passes while this
    // one flips to `Ok`. That single-site revert is why this row exists and why the
    // arms above cannot stand in for it.
    //
    // REACH-GUARD FIRST: `DirectCurrentRaw` deliberately SKIPS the legacy migrations, so
    // if this fixture could not decode bare at all, the `Err` below would prove nothing
    // about the bound.
    assert!(
        serde_json::from_value::<GameState>(base.clone()).is_ok(),
        "reach-guard: the unmutated fixture must decode through the bare-GameState \
         ingress, or the zero-bound Err below is not attributable to the bound"
    );

    let bare_zero = serde_json::from_value::<GameState>(with_bound(0));
    let bare_message = bare_zero
        .expect_err("the bare-GameState ingress must refuse a wire max_iterations of 0 too")
        .to_string();
    assert!(
        bare_message.contains("max_iterations 0"),
        "the bare-ingress rejection must NAME the same invariant, got: {bare_message}"
    );
    assert!(
        serde_json::from_value::<GameState>(with_bound(5)).is_ok(),
        "a legal bound must still load through the bare-GameState ingress"
    );
}

/// R0e — the wire pair NO PRODUCER MINTS: a persisted `LoopShortcut` offer that NARROWS its
/// repetition bound (`schema.is_bounded()`) while recording the PROPOSER'S OWN driving period
/// (`loop_period_controller() == Some(proposer)`) must fail the load.
///
/// The engine's three mints partition that cross-product and none lands in this cell: the
/// object-growth and Path A drain mints both publish `MAX_SHORTCUT_CYCLES` (never
/// `is_bounded()`), and the bounded mint's gate (1b) refuses `ProposerHasDrivingPeriod`. Accepting
/// the pair anyway routes the accepted proposal through `materialize_fixed_shortcut`'s
/// period-ownership early return into `materialize_object_growth_shortcut` — the table agreed to
/// `n` cycles and gets NONE.
///
/// ⚠ NOT A CR REFUSAL, and the row asserts on the engine-invariant message accordingly. CR 732.2a's
/// Example is a proposer repeating THEIR OWN activation a specified 999,999 more times, so this
/// state class is legal at the table; what it violates is producer reachability in this engine.
///
/// THE PERIOD IS A PRODUCTION-SERIALIZED VALUE lifted whole out of the real object-growth capture,
/// never hand-authored JSON — the discipline the `frames_per_period` row above states.
///
/// MATCHED REVERT-PROBE TABLE — each conjunct has its own failing arm, and the three
/// single-conjunct reverts produce three DISTINCT failing sets:
///
/// | mutation to `reject_zero_bound_shortcut_offer` | flips | stays green |
/// |---|---|---|
/// | delete the whole own-period `if` block | A1 → `Ok` | A2, A3, A4, A5, A6 |
/// | delete `schema.is_bounded() &&` | A3, A5 → `Err` | A1, A2, A4, A6 |
/// | delete `&& loop_period_controller() == …` | A2, A4 → `Err` | A1, A3, A5, A6 |
/// | hoist the block ABOVE the `max_iterations == 0` block | A6's message | A1–A5 |
#[test]
fn a_wire_bounded_offer_carrying_the_proposers_own_period_fails_the_load() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let base = envelope["gameState"].clone();

    // ── REACH-GUARDS ON THE BASE: both splices below must CREATE their key ──────────────
    assert_eq!(
        base["waiting_for"]["type"].as_str(),
        Some("LoopShortcut"),
        "the invariant is scoped to the one variant that carries a schema AND a proposer"
    );
    assert!(
        base["waiting_for"]["data"]["schema"].is_object(),
        "the tenacity offer carries a schema object for the bound to live on"
    );
    assert!(
        base["waiting_for"]["data"]["schema"]
            .get("max_iterations")
            .is_none(),
        "the fixture predates the field, so the bound splice CREATES the key (absent ⇒ \
         MAX_SHORTCUT_CYCLES ⇒ NOT is_bounded, which is what arms A3/A5 rest on)"
    );
    assert!(
        base.get("last_loop_action_sequence").is_none(),
        "the fixture records no driving period, so the period splice CREATES the key"
    );
    let proposer = base["waiting_for"]["data"]["proposer"].clone();

    let donor_json = gunzip_dump(include_bytes!(
        "../fixtures/combo_infinite_pile_4p_offer.json.gz"
    ));
    // The combo capture is BARE (no `gameState` envelope) — it is the other decode ingress, and
    // A5 rides it as itself below.
    let donor_state: serde_json::Value =
        serde_json::from_str(&donor_json).expect("the combo dump parses as JSON");
    let donor_period = donor_state["last_loop_action_sequence"].clone();
    let donor_steps = donor_period
        .as_array()
        .expect("the real object-growth capture records a driving period to donate");
    assert!(
        !donor_steps.is_empty(),
        "an empty donated sequence would make loop_period_controller() None and every arm vacuous"
    );
    assert!(
        donor_steps
            .iter()
            .all(|step| step["controller"] == proposer),
        "the donated period must belong to the SAME seat as the tenacity proposer, or A1 would \
         be testing a FOREIGN period — which is A4's job, not A1's"
    );

    let spliced = |bound: Option<u64>, period: Option<&serde_json::Value>| {
        let mut v = base.clone();
        if let Some(n) = bound {
            v["waiting_for"]["data"]["schema"]["max_iterations"] = serde_json::json!(n);
            assert_eq!(
                v["waiting_for"]["data"]["schema"]["max_iterations"].as_u64(),
                Some(n),
                "the bound splice must reach schema.max_iterations"
            );
        }
        if let Some(seq) = period {
            v["last_loop_action_sequence"] = seq.clone();
            assert_eq!(
                &v["last_loop_action_sequence"], seq,
                "the period splice must reach last_loop_action_sequence"
            );
        }
        v
    };
    let decode_persisted = |value: serde_json::Value| {
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(value)
    };

    // ── A1 — THE GUARD FIRES. Also the reach-guard for A2/A3/A4: the predicate reads the period
    // from the state AS DECODED FROM THE WIRE, so an `Err` here is proof the splice landed and
    // survived `decode_persisted_resolution_state`. Were it dropped, this would be `Ok` and the
    // three `Ok` arms below would mean nothing.
    let message = decode_persisted(spliced(Some(5), Some(&donor_period)))
        .expect_err("a narrowed bound carrying the proposer's own period must fail the load")
        .to_string();
    assert!(
        message.contains("narrows its repetition bound"),
        "the rejection must NAME the invariant it enforces and must not be either sibling zero \
         guard firing instead, got: {message}"
    );

    // ── A6 — ORDERING PROBE. `0 < MAX_SHORTCUT_CYCLES`, so a zero bound is ALSO `is_bounded()`:
    // the two blocks are not disjoint and the zero check must keep answering first. No pre-existing
    // row observes this — the sibling zero row's fixture carries no period, so the new predicate is
    // false there regardless of order.
    let message = decode_persisted(spliced(Some(0), Some(&donor_period)))
        .expect_err("a zero bound must still fail the load when a period rides with it")
        .to_string();
    assert!(
        message.contains("max_iterations 0"),
        "ORDERING: hoisting the own-period block above the zero-bound block relabels a corrupt \
         zero with the wrong invariant, got: {message}"
    );

    // ── A2 — THE PERIOD CONJUNCT. A narrowed bound ALONE is the ordinary bounded offer.
    assert!(
        decode_persisted(spliced(Some(5), None)).is_ok(),
        "a narrowed bound with NO recorded period is exactly what the bounded mint publishes"
    );

    // ── A3 — THE `is_bounded()` CONJUNCT. Own period ALONE is the object-growth route's own
    // admission condition; rejecting it would refuse every legitimate growth capture.
    assert!(
        decode_persisted(spliced(None, Some(&donor_period))).is_ok(),
        "an UNNARROWED offer (absent bound ⇒ MAX_SHORTCUT_CYCLES) carrying the proposer's own \
         period is the legitimate object-growth shape and must still load"
    );

    // ── A4 — SEAT-RELATIVITY. It must be THIS proposer's period, not merely A period.
    let foreign_period = {
        let mut seq = donor_period.clone();
        for step in seq
            .as_array_mut()
            .expect("the donated period is an array of steps")
        {
            step["controller"] = serde_json::json!(1);
        }
        assert!(
            seq.as_array()
                .expect("still an array")
                .iter()
                .all(|step| step["controller"] != proposer),
            "the controller rewrite must reach every step, or A4 would re-run A1"
        );
        seq
    };
    assert!(
        decode_persisted(spliced(Some(5), Some(&foreign_period))).is_ok(),
        "a period recorded from a DIFFERENT seat describes no sequence this proposer can take \
         (SITE B's seat-relative form), so it must not reject the offer"
    );

    // ── A5 — THE REAL OBJECT-GROWTH CAPTURE, UNMUTATED, ON THE OTHER GUARDED INGRESS ──────
    // `reject_zero_bound_shortcut_offer` is called from BOTH decoders; A1-A4 ride
    // `decode_persisted_resolution_state`, this one rides `GameStateDecode::decode` through
    // `impl Deserialize for GameState`.
    let combo: GameState = serde_json::from_str(&donor_json)
        .expect("the real object-growth capture must still load through the bare ingress");
    // REACH-GUARD, INLINE — A1 cannot stand in for it (different fixture, different ingress).
    // Without these three, `Ok` would also be explained by the period never surviving THIS
    // decode, and the `is_bounded()` revert (delete it ⇒ this arm must flip to `Err`) would not
    // fire.
    let WaitingFor::LoopShortcut {
        proposer: combo_proposer,
        schema,
        ..
    } = &combo.waiting_for
    else {
        panic!(
            "fixture precondition: the combo capture is AT a LoopShortcut offer, got {:?}",
            combo.waiting_for
        )
    };
    assert!(
        !combo.last_loop_action_sequence.is_empty(),
        "the period must SURVIVE this decode, or A5's Ok is unattributable"
    );
    assert!(
        combo
            .last_loop_action_sequence
            .iter()
            .all(|step| step.controller == *combo_proposer),
        "the surviving period must be homogeneous on the PROPOSER's seat — that is what makes \
         loop_period_controller() == Some(proposer) and puts this arm on the guard's own predicate"
    );
    assert!(
        !schema.is_bounded(),
        "and the offer must be UNNARROWED, so A5's Ok is attributable to the is_bounded() \
         conjunct alone rather than to a missing period"
    );
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
///
/// ⚠ THE `pin` PREFERENCE IS INERT ON EVERY TRACKED DUMP, and that is MEASURED, not
/// inferred from this function's body. Driving all five tracked 4p dumps for 60 beats each:
/// only `dellian_emblem_conqueror_4p` reaches a `WaitingFor::TriggerTargetSelection` window
/// at all (seven of them, at beats 0/9/18/27/36/45/54, all on `ObjectId(541)`), and every
/// one of those windows enumerates **`GameAction::ChooseTarget` ×3 and
/// `GameAction::SelectTargets` ×0** — so the `SelectTargets` preference above never fires
/// and the fallback answers with a `ChooseTarget`. `dina`, `tenacity`,
/// `witherbloom_sprout_lumaret` and `witherbloom_sprout_lumaret_simple` reach NO
/// `TriggerTargetSelection` window in 60 beats. This is the same trap
/// `fantastic_four_bounded_loop.rs` already records for the F4 dump, and it means NO TRACKED
/// FIXTURE CAN EXERCISE THE `SelectTargets` REDUCER ARM at the wire tier — see
/// [`c2a_row_t1b_both_trigger_target_selection_arms_route_through_the_single_writer`].
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

    // (i) the `pass_priority_once_with_pipeline` sampler half (its `is_forced_cascade_window`
    //     clear arm): a forced pre-priority window observed with an
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

/// PR-7 Phase 5a — CR 732.2a per-iteration pin enumeration, on a REAL 4p board.
///
/// `bounded_cycle_pin_slots` is the single authority for the choice slots a bounded cycle
/// offer must publish. Dump B is the acceptance population: obj **541** is a CR 114.2
/// emblem (command zone, "both owned and controlled by that player") whose triggered
/// ability drains `target opponent`, i.e. the
/// `Typed{type_filters: [], controller: Opponent, properties: []}` player shape.
///
/// **TWO ARMS, and the pairing is the whole point.** The dump ships with the prompt UP
/// (`TriggerTargetSelection` carrying already-materialized `legal_targets`), so arm ⓐ alone
/// would pass against a prompt-READING implementation — which returns ZERO slots at the
/// real offer beat, where `waiting_for` is `Priority`. Arm ⓑ is the identical state with
/// exactly one field reassigned.
///
/// REVERT-PROBES (both must flip):
/// * ⓘ narrow the AST predicate so `Typed{[], Opponent, []}` is rejected ⇒ BOTH arms return
///   zero ⇒ FAILS.
/// * ⓙ re-implement the enumerator to read `state.waiting_for`'s
///   `target_slots[..].legal_targets` ⇒ arm ⓐ still passes, arm ⓑ returns zero ⇒ FAILS.
/// * ⓐ' delete the `entry.controller != proposer` filter ⇒ the bystander-proposer
///   assertion FAILS. (On THIS board every one of the 152 stack entries is P0-controlled —
///   measured — so the honest form of that probe is a bystander proposer, not a rising
///   count for P0.)
///
/// MUST-NOT-FLIP: `bounded_cycle_pin_slots(..).is_empty()` on the shipped
/// `b3_materialize_stop_short` offer board — asserted, not assumed. That zero is the
/// byte-identity pin for every shipped `Fixed(N)` drive, and pairing it with dump B's
/// non-zero in the SAME row proves the instrument returns both values.
#[test]
fn bounded_cycle_pin_slots_enumerates_the_emblem_slot() {
    use engine::game::engine::bounded_cycle_pin_slots;
    use engine::types::zones::Zone;

    const EMBLEM: ObjectId = ObjectId(541);
    const P3: PlayerId = PlayerId(3);

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    )));

    // ── reach guards, all derived from the loaded board, none from the predicate ──
    let emblem = state
        .objects
        .get(&EMBLEM)
        .expect("reach-guard: dump B carries the emblem object");
    assert_eq!(
        emblem.zone,
        Zone::Command,
        "reach-guard: CR 114.2 puts the emblem in the command zone — the whole reason this \
         row exists (a battlefield-only slot builder would be untested by it)"
    );
    let emblem_incarnation = emblem.incarnation;
    let emblem_entries = state.stack.iter().filter(|e| e.source_id == EMBLEM).count();
    assert_eq!(
        emblem_entries, 1,
        "reach-guard, measured off the loaded 152-deep stack: the emblem has exactly one \
         live entry here, so this row's COUNT carries no claim about the per-SOURCE dedupe \
         — that is \
         `bounded_cycle_pin_slots_publishes_one_point_per_source_not_per_entry`'s job"
    );
    assert_eq!(
        engine_live_opponents(&state, P0),
        vec![P1, P2, P3],
        "reach-guard: three living opponents, so the per-iteration choice is REAL"
    );

    let expected_slot = DecisionSlot {
        source: YieldTarget::ThisObject {
            source_id: EMBLEM,
            incarnation: Some(emblem_incarnation),
            trigger_description: None,
        },
        index: 0,
    };
    let expected_legal = vec![
        TargetRef::Player(P1),
        TargetRef::Player(P2),
        TargetRef::Player(P3),
    ];

    // ── arm ⓐ: the shipped board, prompt UP ──
    assert!(
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
        "arm ⓐ precondition: the dump ships AT the prompt; got {:?}",
        state.waiting_for
    );
    let with_prompt = bounded_cycle_pin_slots(&state, P0);
    // The CR 115.2 TARGETS half — this row's subject. Filtered rather than counted whole
    // because the mint also admits shape (B) (may-only, no announcement choice), whose
    // points are asserted separately below; before shape (B) existed the two coincided.
    let targets: Vec<_> = with_prompt
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .collect();
    assert_eq!(
        targets.len(),
        emblem_entries,
        "one TARGETS point per qualifying SOURCE; on this board that count coincides with \
         the emblem's single entry, which is asserted above rather than assumed"
    );
    for point in &targets {
        assert_eq!(
            point.slot, expected_slot,
            "the slot names the CR 114.2 command-zone emblem at its CR 400.7 incarnation"
        );
        assert_eq!(
            point.kind,
            DecisionPointKind::Targets {
                legal_targets: expected_legal.clone(),
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
            "the legal set is `find_legal_targets`' native output (CR 115.2), not a \
             declaration echo"
        );
    }
    // ── the shape-(B) half, measured on the SAME board rather than asserted elsewhere ──
    // CR 603.5: three P0-controlled optional no-target triggers (sources 126, 208, 274)
    // publish a `MayChoice` gate each. 126 and 208 carry 34 stack entries apiece here, so
    // this count is ALSO the per-source dedupe on a real board: without it the shipped
    // dump would publish 69 may points, not 3.
    let may_sources: Vec<_> = with_prompt
        .iter()
        .filter(|p| p.kind == DecisionPointKind::MayChoice)
        .map(|p| match &p.slot.source {
            YieldTarget::ThisObject { source_id, .. } => *source_id,
            other => panic!("a mint slot names an object source; got {other:?}"),
        })
        .collect();
    assert_eq!(
        may_sources,
        vec![ObjectId(126), ObjectId(208), ObjectId(274)],
        "CR 603.5: the may-only (shape (B)) sources this board publishes, deduped per \
         SOURCE across their 34/34/1 stack entries"
    );
    for src in [ObjectId(126), ObjectId(208)] {
        assert!(
            state.stack.iter().filter(|e| e.source_id == src).count() > 1,
            "reach-guard: {src:?} must carry MORE than one entry, or the dedupe assertion \
             above is vacuous"
        );
    }

    // ── arm ⓑ: the SAME state at the real offer beat — one field reassigned ──
    state.waiting_for = WaitingFor::Priority { player: P0 };
    assert_eq!(
        bounded_cycle_pin_slots(&state, P0),
        with_prompt,
        "arm ⓑ: byte-for-byte the same slot set with NO prompt to read. Both production \
         call sites run at `WaitingFor::Priority`, so an implementation that reads \
         `target_slots[..].legal_targets` publishes nothing when it matters"
    );

    // ── ⓐ': a bystander proposer specifies none of these choices (CR 732.2a) ──
    assert!(
        state.stack.iter().all(|e| e.controller == P0),
        "measured: every dump-B stack entry is P0-controlled, so the controller filter is \
         probed from the PROPOSER side"
    );
    for bystander in [P1, P2, P3] {
        assert!(
            bounded_cycle_pin_slots(&state, bystander).is_empty(),
            "a bystander ({bystander:?}) controls none of these entries"
        );
    }

    // ── must-NOT-flip: the shipped Fixed(N) drive board publishes NOTHING ──
    let (shipped, _l0, _cleric) = reach_2p_optional_drain_offer();
    assert!(
        bounded_cycle_pin_slots(shipped.state(), P0).is_empty(),
        "the untargeted `each opponent loses 1 life` drain reifies no per-iteration player \
         choice — every shipped Fixed(N) drive must stay byte-identical"
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
        beats, 8,
        "the `c decline` arm re-derives the crown after a measured 8 driven beats — a 0 here \
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
            declaration: None,
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
                // The witness reports on the basis-B turn conjunct, which reads the
                // CR 104.4b comparand half.
                let front = state.loop_detect_ring.front()?.normalized.clone();
                let back = state.loop_detect_ring.back()?.normalized.clone();
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
                front.clone(),
                back.clone(),
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
// R6a — the ∞ badge stays up while a collapse is merely SCHEDULED (the engine defers APPLYING
// an accepted shortcut's growth to the CR 500.5 boundary, while advancing to the proposal's
// ending point per CR 732.2c), and CR 732.2c bounds the boundary prompt by the accepted count.
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

/// R6a-1 (PRIMARY), INVERTED to option (B). Accepting the Witherbloom/Sprout loop writes
/// `unbounded_resources = {P0: [Life(0), TokensCreated]}` plus a non-empty ∞ pile, and
/// registers a finite collapse. The COUNT is fixed at accept (`pending_materialization_count`,
/// which bounds the boundary prompt per CR 732.2c); what this engine defers is APPLYING it, until
/// the CR 500.5 boundary (`game::turns`), while the game advances to the proposal's ending point
/// (CR 732.2c; `types::game_state`'s `scheduled_collapse_axes` doc has the full reading). This test
/// pins what the engine DOES during that window: the marks and their enablers are still live, so the
/// projection KEEPS every `∞` surface. CR 732.2c bounds the collapse; it never licensed hiding a
/// mark the store still carries, which is what the BASE gate used it for.
///
/// NEVER HIDE — and never filter the store either. The store must still carry the mark
/// (the engine-state enabler lockstep and `zones::apply_zone_exit_cleanup`'s defuse read it until
/// the boundary applies the growth), so this row asserts store AND wire.
///
/// NON-VACUITY: every wire assertion here is a NON-emptiness paired with the store's own
/// non-emptiness at (1), so a projection that returned nothing fails immediately.
///
/// ASSERTION ORDER inside the viewer loop is PILE FIRST, then rows: RP-1 (pile guard) and
/// RP-1d (row guard) each panic on their own line, so whichever comes first hides the other.
/// Pile-first buys RP-1d an in-test rows→pile control; the rows control RP-1 loses here is
/// supplied out-of-loop by `unregistered_axis_still_renders_its_infinity_badge`, whose
/// pre-clear rows assertion is green under RP-1 and red under RP-1d.
///
/// REVERT-PROBES (RUN):
/// ⓐ RP-1 — restore `if collapse_scheduled(controller, &TokensCreated) { continue; }` in
///    `derive_views`' pile loop ⇒ the PILE assertion below FAILS; the rows assertion is
///    unreached here and is controlled out-of-loop (above).
/// ⓑ RP-1d — restore `if collapse_scheduled(controller, &axis) { continue; }` in the resource
///    row loop ⇒ the ROWS assertion below FAILS while the PILE assertion above it passes.
#[test]
fn scheduled_collapse_still_renders_the_unbounded_badge() {
    let mut state = r6a_offer_state();

    // (0) reach-guard: the real cast reached the CR 732.2a offer.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the buyback+convoke recast must surface P0's offer, got {:?}",
        state.waiting_for
    );

    // BASELINE, captured BEFORE the accept so the unmaterialized claim below is falsifiable.
    // A `life > 0` assertion would also pass AFTER materialization (200 accepted gains would leave
    // life well above 0), so it could not distinguish the state this test exists to pin.
    let life_before = state.players.iter().find(|p| p.id == P0).unwrap().life;

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
    // The growth is UNMATERIALIZED: the accepted count has not been applied, so P0's life is
    // EXACTLY what it was before the accept. The ∞ row beside it reports the live loop mark, not
    // the current total. Asserting EQUALITY against the pre-accept baseline (not `> 0`) is what
    // makes this row discriminating: a premature materialization of the accepted 200 Life(P0)
    // gains moves this number and reds the row, whereas `life > 0` survives it.
    let life = state.players.iter().find(|p| p.id == P0).unwrap().life;
    assert_eq!(
        life, life_before,
        "the ∞-badged Life(P0) axis must be UNMATERIALIZED at this point — life must equal its \
         pre-accept baseline, got {life} vs {life_before}"
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

    // (3) DISCRIMINATOR — on the WIRE, for EVERY viewer (and the spectator view), the ∞ pile and
    // both ∞ rows still project. No ∞ surface consults the collapse schedule, so the HUD can never
    // show a card group's ∞ while hiding its resource badge. The PER-SURFACE positive rows live on
    // their own real fixtures —
    // `combo_infinite_pile::real_4p_object_growth_accept_writes_infinite_pile` (pile) and
    // `kilo_live_offer_from_real_dump::kilo_accept_marks_pentad_charge_as_unbounded_display_
    // target` (counter pills) — so a regression on ONE surface stays visible even though this
    // row covers pile + rows at once.
    for viewer in [None, Some(P0), Some(P1), Some(P2), Some(PlayerId(3))] {
        let views = engine::game::derived_views::derive_views(&state, viewer);
        assert!(
            !views.unbounded_pile.is_empty(),
            "the scheduled collapse still projects the ∞ pile (viewer {viewer:?})"
        );
        let axes: Vec<ResourceAxis> = views.unbounded_resources.iter().map(|r| r.axis).collect();
        assert!(
            axes.contains(&ResourceAxis::Life(P0)) && axes.contains(&ResourceAxis::TokensCreated),
            "...and both ∞ rows beside it (viewer {viewer:?}), got {axes:?}"
        );
    }

    // (3b) A TRIPWIRE, NOT A SECOND PRODUCER. Multiplayer broadcasts (`phase-server`) and the
    // WASM `wrap_filtered` getter go through `derive_filtered_views`, which CALLS
    // `derive_views(filtered_state, viewer)` and then overrides only
    // `unique_authorized_submitter` and `blocker_assignment_pairs`. It WRAPS; it does not
    // bypass. So `derive_views` alone decides what the broadcast path shows — there is no other
    // producer of these three fields, and this row costs zero production code.
    //
    // What it DOES guard is the INPUT: `filter_state_for_viewer` is a clone-and-redact with
    // ZERO `unbounded` references today, so a filtered viewer sees the same ∞ surfaces the
    // hot-seat viewer does. If a future redaction ever dropped the ∞ stores from the filtered
    // clone, the broadcast path alone would go dark — remote players would lose the ∞ pile and
    // rows while the local viewer kept them. That asymmetry is the regression this row catches.
    // EXACT MEMBERSHIP, not non-emptiness. A non-empty check passes a PARTIAL projection that
    // drops one of the two axes or loses pile members, which is precisely the regression described
    // above. These rows pin the SET and the FULL membership, both compared against the store so the
    // expectation cannot drift away from what the accept actually wrote.
    let expected_axes: std::collections::BTreeSet<ResourceAxis> = marked.iter().copied().collect();
    // CR 110.1: the projection emits only pile members still ON THE BATTLEFIELD, so the oracle
    // must model that filter. A legitimately stale STORED id (the store is deliberately
    // unfiltered) is the projection being RIGHT, not a regression — without this filter the
    // equality below would indict the correct behaviour. On this fixture every stored member is
    // still on the battlefield here, so the filter is a no-op TODAY: it is latent correctness,
    // and `stale_pile_member_is_omitted_from_the_wire_but_kept_in_the_store` below is the case
    // that actually makes the stale/live distinction bite.
    let expected_pile: std::collections::BTreeSet<ObjectId> = state
        .unbounded_loop_pile
        .values()
        .flat_map(|ids| ids.iter().copied())
        .filter(|id| state.battlefield.contains(id))
        .collect();
    assert!(
        expected_axes.len() >= 2 && !expected_pile.is_empty(),
        "control: the expectations themselves must be non-trivial, got {expected_axes:?} / \
         {} pile members",
        expected_pile.len()
    );
    for viewer in [P0, P1, P2, PlayerId(3)] {
        let filtered = engine::game::visibility::filter_state_for_viewer(&state, viewer);
        let views =
            engine::game::derived_views::derive_filtered_views(&state, &filtered, Some(viewer));
        let got_axes: std::collections::BTreeSet<ResourceAxis> =
            views.unbounded_resources.iter().map(|r| r.axis).collect();
        assert_eq!(
            got_axes, expected_axes,
            "the viewer-FILTERED broadcast path must project EVERY marked ∞ axis, not merely some \
             (viewer {viewer:?})"
        );
        let got_pile: std::collections::BTreeSet<ObjectId> =
            views.unbounded_pile.iter().copied().collect();
        assert_eq!(
            got_pile, expected_pile,
            "the viewer-FILTERED broadcast path must project the FULL ∞ pile membership \
             (viewer {viewer:?})"
        );

        // NOTE ON WHAT COVERS "flagging must not FILTER rows": the `got_axes == expected_axes`
        // assertion above is exact-set equality against every marked axis, so it already fails if
        // the scheduled flag ever suppressed a row. A separate "a TokensCreated row exists" pin
        // used to sit here and was strictly subsumed by it — no mutation could red the pin without
        // first reding the equality — so it is gone rather than left as decoration. (Rows CAN be
        // dropped, by `object_growth_backing`, for a TOKEN axis whose whole registered pile left
        // the battlefield; this fixture keeps its backing intact, pinned by `expected_pile`.)
        //
        // R2 — the SCHEDULE survives the viewer-filtered broadcast path. Read off
        // `unbounded_families`, the channel that replaced the per-row `scheduled` flag; the
        // certainty CLASS is pinned elsewhere (B-1, W2, M2-a/M2-c), what matters here is that a
        // scheduled family reaches every viewer.
        let scheduled_families: Vec<UnboundedFamily> = views
            .unbounded_families
            .iter()
            .filter(|f| matches!(f.state, FamilyCollapseState::Scheduled { .. }))
            .map(|f| f.family)
            .collect();
        assert!(
            scheduled_families.contains(&UnboundedFamily::Life)
                && scheduled_families.contains(&UnboundedFamily::Tokens),
            "R2/filtered: the filtered broadcast path reports both scheduled families (viewer \
             {viewer:?}), got {:?}",
            views.unbounded_families
        );
        // R1 — the channel survives serialize→deserialize, and a POPULATED channel is EMITTED.
        // `unbounded_families` is `skip_serializing_if = "Vec::is_empty"`, so the emission half is
        // what catches a state that is computed correctly and then silently dropped on the wire.
        let json = serde_json::to_string(&views).expect("serialize");
        let back: engine::game::derived_views::DerivedViews =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.unbounded_families, views.unbounded_families,
            "R1/roundtrip: the family collapse states survive serialize→deserialize (viewer \
             {viewer:?})"
        );
        assert!(
            json.contains("\"unbounded_families\"") && json.contains("\"Scheduled\""),
            "R1/emitted: a populated family channel is EMITTED, not skipped (viewer {viewer:?})"
        );
    }

    // (4) THE STORE IS UNTOUCHED — the projection read, it did not mutate.
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&marked),
        "the ∞ store must survive the projection (the engine-state enabler lockstep + the \
         zone-exit defuse still need it until the boundary)"
    );
    assert!(
        !state.unbounded_loop_pile.is_empty(),
        "the ∞ pile must survive the projection too"
    );
}

/// MED-2 (CR 732.2a + CR 110.1): a pile member that has LEFT the battlefield is omitted from
/// the WIRE while the STORE keeps it. Sibling of the oracle filter added to
/// `scheduled_collapse_still_renders_the_unbounded_badge` above, which is a no-op on that
/// fixture (nothing is stale there) — this test is what makes the distinction bite.
///
/// The store/wire split is the whole contract: `unbounded_loop_pile` must stay unfiltered
/// because the boundary collapse and `zones::apply_zone_exit_cleanup`'s defuse both read it,
/// so the liveness filter has to live in the projection.
///
/// MUTATIONS (RUN, measured over the 164-test loop/∞ blast radius):
/// - Delete `if state.battlefield.contains(id)` from `derive_views`' pile loop ⇒ row (1) reds
///   here ("the wire omits the stale member (viewer None)") and NOTHING else moves — 1 failed
///   / 163 passed. That zero collateral is the finding, not a footnote: before this test, the
///   pile loop's liveness filter had no runnable guard at all. In particular the oracle filter
///   added to `scheduled_collapse_still_renders_the_unbounded_badge` stays GREEN under this
///   mutation, because no member is stale on that fixture — the filter there is latent
///   correctness, and THIS test is what makes the distinction bite.
/// - "Fix" it by pruning the STORE instead of the wire (drop the departing id from
///   `unbounded_loop_pile` in `zones::apply_zone_exit_cleanup`) ⇒ rows (1), (2) and (4) go
///   GREEN and only row (3) reds. Row (3) is the discriminator against that wrong fix.
#[test]
fn stale_pile_member_is_omitted_from_the_wire_but_kept_in_the_store() {
    use engine::game::zones::move_to_zone;
    use engine::types::zones::Zone;
    use std::collections::BTreeSet;

    let mut state = r6a_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 200);

    let stored: BTreeSet<ObjectId> = state
        .unbounded_loop_pile
        .get(&P0)
        .expect("the object-growth accept registers a ∞ pile")
        .clone();
    assert!(
        stored.len() >= 2,
        "reach-guard: this rig's pile has >= 2 members, so removing ONE leaves a non-empty \
         wire — the case is about a STALE member, not about the whole backing set dying. The \
         whole-set case is `accepted_object_growth_row_survives_losing_its_entire_pile`, which \
         asserts the row SURVIVES it, because that rig's collapse has been accepted (CR 732.2c); \
         got {}",
        stored.len()
    );
    assert!(
        stored.iter().all(|id| state.battlefield.contains(id)),
        "reach-guard: BEFORE the departure every stored member is on the battlefield, so the \
         wire/store divergence below is caused by the departure and nothing else"
    );

    let departed = *stored.iter().next().unwrap();
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(&mut state, departed, Zone::Graveyard, &mut events);
    assert!(
        !state.battlefield.contains(&departed),
        "the departure really happened (CR 110.1: it stopped being a permanent)"
    );

    // (3) THE STORE IS NOT FILTERED. This is the discriminator against a "fix" that prunes
    //     `unbounded_loop_pile` itself: that mutation satisfies (1), (2) and (4) and reds only
    //     this row. The store must survive because the boundary collapse and the zone-exit
    //     defuse both read it.
    assert!(
        state
            .unbounded_loop_pile
            .get(&P0)
            .is_some_and(|pile| pile.contains(&departed)),
        "(3) the STORE must still carry the departed member — only the wire filters"
    );

    let expected: BTreeSet<ObjectId> = stored
        .iter()
        .copied()
        .filter(|id| state.battlefield.contains(id))
        .collect();
    assert_eq!(
        expected.len(),
        stored.len() - 1,
        "control: exactly ONE stored member became stale"
    );

    for viewer in [None, Some(P0), Some(P1), Some(P2), Some(PlayerId(3))] {
        let views = engine::game::derived_views::derive_views(&state, viewer);
        let wire: BTreeSet<ObjectId> = views.unbounded_pile.iter().copied().collect();
        assert!(
            !wire.contains(&departed),
            "(1) the wire omits the stale member (viewer {viewer:?})"
        );
        assert_eq!(
            wire, expected,
            "(2) EXACT membership: the wire is stored ∩ battlefield, so a projection that \
             dropped EXTRA members fails here too (viewer {viewer:?})"
        );
        assert_eq!(
            views.unbounded_pile.len(),
            stored.len() - 1,
            "(4) exactly one member is lost between store and wire (viewer {viewer:?})"
        );
    }

    // The ROW survives: the rest of the pile still backs the axis. This is the `Some(true)`
    // arm of `derived_views::object_growth_backing` — a partial departure is not a revocation.
    let axes: Vec<ResourceAxis> = engine::game::derived_views::derive_views(&state, None)
        .unbounded_resources
        .iter()
        .map(|row| row.axis)
        .collect();
    assert!(
        axes.contains(&ResourceAxis::TokensCreated),
        "one stale member leaves live backing behind, so the ∞ row persists, got {axes:?}"
    );
}

/// R6a-3 (FAIL-CLOSED), under option (B). ONE rig, TWO arms that fail on DIFFERENT wrong
/// implementations — the pair is what pins "the ∞ rows do not depend on the collapse schedule at
/// all", which is strictly stronger than either arm alone.
///
/// 1. PRE-CLEAR arm (stash PRESENT) — kills any STASH-KEYED hide filter. This is also the
///    load-bearing rows control for the sibling test above: it is green under the pile-guard probe
///    (RP-1) and red under the row-guard probe (RP-1d), i.e. it discriminates in BOTH directions,
///    which is what lets that test assert pile-first.
/// 2. POST-CLEAR arm (stash DROPPED, marks kept) — kills any LABELLABILITY-KEYED hide filter.
///    `LoopCollapseAxis::from_resource_axis` maps `TokensCreated` / `Counter(..)` / `Life(..)` to
///    a label, so "hide every axis that has a collapse label" is a one-liner that passes arm 1's
///    sibling rows and still hides an axis nothing will ever collapse. Arm 2 is the only row in
///    this file that reds it.
///
/// REVERT-PROBE (RP-1d, RUN): restore `if collapse_scheduled(controller, &axis) { continue; }` in
/// `derive_views`' resource-row loop ⇒ arm 1 FAILS (stash present ⇒ rows hidden) while arm 2 stays
/// green (stash cleared ⇒ nothing scheduled ⇒ rows project). That asymmetry is why both arms
/// exist.
///
/// REVERT-PROBE (RP-4, RUN): hide rows matching
/// `matches!(axis, ResourceAxis::TokensCreated | ResourceAxis::Counter(..) | ResourceAxis::Life(_))`
/// — a *transcription* of the three `Some` arms of `LoopCollapseAxis::from_resource_axis`
/// (`types/game_state.rs`), inlined because that fn is declared as a bare module-private
/// `fn` and `game::derived_views` is a sibling module, so calling it is
/// `error[E0624]: associated function from_resource_axis is private`. Do not widen its
/// visibility. ⇒ BOTH arms FAIL, and arm 2 is the one that is unreachable by any stash-keyed
/// probe, because the stash is already gone when it runs.
#[test]
fn unregistered_axis_still_renders_its_infinity_badge() {
    let mut state = r6a_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 200);

    // Both arms need the STORE marks present; arm 1 additionally needs the registration present.
    let marked = state
        .unbounded_resources
        .get(&P0)
        .expect("accept marked the ∞ axes")
        .clone();
    assert!(
        marked.contains(&ResourceAxis::TokensCreated) && marked.contains(&ResourceAxis::Life(P0)),
        "reach-guard: both labellable axes are marked, got {marked:?}"
    );
    assert_eq!(
        state.pending_unbounded_materialization.len(),
        1,
        "reach-guard for arm 1: the registration is present, so a stash-keyed filter would fire"
    );

    // ARM 1, BEFORE the drop: while the collapse is merely SCHEDULED the ∞ rows stay projected.
    let scheduled_rows =
        engine::game::derived_views::derive_views(&state, None).unbounded_resources;
    let scheduled_axes: Vec<ResourceAxis> = scheduled_rows.iter().map(|r| r.axis).collect();
    assert!(
        scheduled_axes.contains(&ResourceAxis::TokensCreated)
            && scheduled_axes.contains(&ResourceAxis::Life(P0)),
        "a merely-SCHEDULED collapse still projects both ∞ rows, got {scheduled_axes:?}"
    );

    // R3 PRE-CLEAR positive control — without it the post-clear "every family Unscheduled" below
    // is VACUOUS: a mutant that never reports a schedule at all satisfies the post-clear
    // assertion, so the pair only discriminates because this arm proves a schedule CAN be reported
    // on this same state.
    {
        let v = engine::game::derived_views::derive_views(&state, None);
        let j = serde_json::to_string(&v).unwrap();
        assert!(
            v.unbounded_families
                .iter()
                .any(|f| matches!(f.state, FamilyCollapseState::Scheduled { .. }))
                && j.contains("\"Scheduled\""),
            "R3/pre-clear: a registered materialization SCHEDULES a family AND emits it, got {:?}",
            v.unbounded_families
        );
    }

    // ARM 2: drop the registrations, keep the marks — an ∞ axis that is collapsible-LABELLED but
    // has nothing scheduled to collapse it.
    state.pending_unbounded_materialization.clear();

    // R3 — with nothing scheduled EVERY family is `Unscheduled`, which is deliberately NOT the
    // same as "the channel disappears": the ∞ badges are still on screen, they just promise
    // nothing. The rows must therefore still be there, and the `Scheduled` encoding must be gone
    // from the wire entirely.
    {
        let v = engine::game::derived_views::derive_views(&state, None);
        let j = serde_json::to_string(&v).unwrap();
        assert!(
            !v.unbounded_families.is_empty()
                && v.unbounded_families
                    .iter()
                    .all(|f| f.state == FamilyCollapseState::Unscheduled),
            "R3/post-clear: with nothing scheduled every family is Unscheduled — and the channel \
             is still populated, so this is not vacuous; got {:?}",
            v.unbounded_families
        );
        assert!(
            !j.contains("\"Scheduled\""),
            "R3/post-clear: no Scheduled state reaches the wire, got {j}"
        );
        let back: engine::game::derived_views::DerivedViews = serde_json::from_str(&j).unwrap();
        assert_eq!(
            back.unbounded_families, v.unbounded_families,
            "R3/default: the Unscheduled channel round-trips unchanged"
        );
        assert!(
            !back.unbounded_resources.is_empty(),
            "R3/default reach: …and it still carries the rows, so the line above is not vacuous"
        );
    }

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

// ===========================================================================
// PR-7 Phase 5b — CR 732.2a BOUNDED cycle fast-forward, on a REAL 4p dump.
//
// The class Path A and Path B both refuse: a drain lethal to SOME opponents leaves a
// second non-faller, so CR 104.2a determinacy (`loop_check`'s crown gate) will not crown,
// and a life-loss axis is not a CR 732.4 no-loss draw. Every row below LOADS
// `dina_conqueror_4p.json.gz` through the production restore chokepoint and DRIVES real
// beats through `apply()` — the offer is an accumulation across dozens of them, which no
// synthetic `GameScenario` reproduces.
// ===========================================================================

/// Drive the loaded dump until the ENGINE ITSELF writes a bounded offer, and return the
/// state at that beat. Reads `state.waiting_for` — i.e. the production Path D write inside
/// `interactive_loop_bridge` — NEVER an out-of-band call to the offer predicate, which
/// would prove only that the predicate agrees with itself.
fn drive_to_bounded_offer(state: &mut GameState, cap: usize) -> Option<usize> {
    let pin = engine_live_opponents(state, P0).first().copied();
    for beat in 0..cap {
        if matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut {
                predicted_winner: None,
                ..
            }
        ) {
            return Some(beat);
        }
        if dump_drive_one_beat(state, pin).is_err() {
            return None;
        }
    }
    None
}

fn bounded_offer_parts(
    state: &GameState,
) -> (
    PlayerId,
    &engine::analysis::loop_check::LoopCertificate,
    &engine::analysis::decision_template::ShortcutDecisionSchema,
) {
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            predicted_winner: None,
            certificate,
            schema,
            declaration: _,
        } => (*proposer, certificate, schema),
        other => panic!("expected a bounded LoopShortcut offer, got {other:?}"),
    }
}

/// PR-7 Phase 5b acceptance — the bounded offer FIRES on the real 4-player Dina/Conqueror
/// drain, at three living opponents, with a bound computed from the offer-beat board.
///
/// ⚠ CERTIFICATION BASIS — **B**, with a derived `frames_per_period == 1`. An earlier revision
/// of this doc said basis **A** ("direct recurrence"); that was WRONG and the correction is
/// load-bearing, so it is recorded rather than swapped. MEASURED two independent ways:
/// (i) instrumenting the `basis_a` match in `try_offer_bounded_cycle_shortcut` prints
/// `BASIS=B k=1 turn=5 phase=CombatDamage ring=3` at this row's offer beat; (ii) making
/// `ring_delta_signature` return `None` unconditionally removes this row's offer entirely
/// (the drive runs its full 400-beat cap and the `expect` below fires) — which could not
/// happen if basis A were certifying, because `ring_delta_signature` is reached only from the
/// `None =>` arm.
///
/// ⚠ WHY BASIS A REFUSED — the MECHANISM, measured at this row's own offer beat by
/// instrumenting the `basis_a` walk (ring length 3, walked newest-first). Both disjuncts fail,
/// for two DIFFERENT reasons, and neither is the one a reader would guess:
/// * the **equal** disjunct is refused by stack growth. `ring[1] -> current` is
///   `stack[8 -> 10]`: **two more `ObjectId(401)` "Bloodthirsty Conqueror" `GainLife`
///   triggered-ability entries per period** (7 -> 9, alongside one steady `ObjectId(71)`
///   "Dina, Soul Steeper" `LoseLife` entry) — a super-critical mu > 1 cascade, so the board
///   provably never recurs. The single pair that IS `eq == true` (`ring[2]`, `stack[10 -> 10]`)
///   carries a ZERO delta, so `net_progress_for(proposer)` is false and it is discarded.
/// * the **cover** disjunct clears gates (1)-(4) on that same pair and is then vetoed at
///   **gate (5)** — the off-stack fire-time condition guard — by `ObjectId(90)`
///   **"Mortality Spear"** sitting in the **Library**, carrying
///   `ModifyCost { Reduce, {2} }` / `affected: SelfRef` gated on
///   `LifeGainedThisTurn { Controller } >= 1`: a PROJECTED axis read at fire time.
///   The `scope.cast_card_ids` relief that exists for exactly this def shape cannot apply,
///   because step (1b) of the bounded class REQUIRES an empty `last_loop_action_sequence`,
///   so `window_cast_card_ids` returns `None` and gate (5) scans everything. Two
///   individually-correct constraints composing into a refusal neither intended.
///   (On the older `ring[0]` pair cover instead fails at **gate (1)**, on `loop_states_equal`
///   of the stack-cleared projected board — `object_resource_axes_match` was `true` at every
///   gate-(1) refusal measured in this run, so it is NOT the refuser here.)
///
/// THE PUBLISHED PAYLOAD CANNOT DISTINGUISH THE TWO HERE, and that is why the row asserts a
/// structural `frames_per_period >= 1` and not a basis. ⚠ RE-DERIVED IN FIX ROUND 2, because
/// fix round 1 moved the ground under the older wording: basis A no longer publishes a hardcoded
/// `1`, it MEASURES the span from the certifying prior's ring index, and basis B *derives* `k`
/// from `1` upward. Both therefore range over the same values and **NO published value
/// discriminates in either direction** — not `== 1`, and no longer `!= 1` either (a basis-A span
/// of 2 is exactly what `interactive_3p_subset_lethal_does_not_crown` publishes). Any row still
/// reading a basis off this field is stating a necessary-not-sufficient condition at best. No
/// `pub` predicate closes the gap either: basis A's certifying condition is a DISJUNCTION whose
/// second half, `loop_states_cover_modulo_growth_pinned`, is `pub(crate)` and unnameable from
/// an integration test — so the sound attribution stays the discriminating probe (force
/// `ring_delta_signature` to return `None`; basis-B rows lose their offer, basis-A rows keep it).
///
/// CONSEQUENCE, and it is the good one: this row is a basis-B positive control on a REAL 4p
/// dump. It is also therefore SUBJECT TO the CR 703.1 turn-position conjunct rather than
/// bypassing it — the conjunct is evaluated here and PASSES, because the certifying window is
/// `turns[5,5,5] phases[CombatDamage x3] extra[0x3]`. It is a must-NOT-flip in both
/// directions, MEASURED: deleting the conjunct leaves this row green, and keeping it leaves
/// this row green. It is therefore NOT a discriminating control for that conjunct — the rows
/// that carry that discrimination are `analysis::resource`'s
/// `drawgo_turn_structure_yields_no_basis_b_signature` and
/// `ring_delta_signature_certifies_only_a_period_seen_twice` arm ⓕ (refusing side), and
/// `bounded_offer_on_a_within_turn_draw_drain_is_basis_b` (positive side).
///
/// EVERY NUMBER IS COMPUTED IN-TEST from the offer-beat state. The chain's reported "32"
/// is not a fixture fact: it drifts with how many beats the drive takes to accumulate, and
/// this row recomputes `min over living opponents of (life - 1) / per-cycle loss` at the
/// beat the offer actually appeared.
///
/// NON-VACUITY: BASE is a measured NO-OFFER trajectory. Before Path D existed the same
/// drive ran 326 beats on this dump and reached `WaitingFor::LoopShortcut` zero times, so
/// the offer cannot appear here vacuously. The two field-value discriminators are asserted
/// rather than a code location: `predicted_winner == None` (this seam never calls
/// `live_mandatory_loop_winner`, so it cannot have inherited Path A's crown) and
/// `last_loop_action_sequence` EMPTY (the object-growth producer's class is the complement).
///
/// REVERT-PROBES (each must FLIP to FAIL):
/// * delete the Path D block in `interactive_loop_bridge` ⇒ no offer ⇒ the
///   `drive_to_bounded_offer` expect FAILS.
/// * make step (7)'s range check `1..=MAX_SHORTCUT_CYCLES` ⇒ NOTHING flips, anywhere. The
///   claim this bullet used to make — that the `schema.is_bounded()` assertion below "is the
///   one that flips" — was FALSE, and is corrected rather than quietly deleted. RE-MEASURED
///   under that exact mutation, with the runner and the filter both named because the earlier
///   revision quoted a count whose shape did not match the filter beside it (fix round 2,
///   LOW-2): `cargo test -p phase-engine --test integration -- loop_shortcut::` (module filter on the
///   `integration` binary) ⇒ **85 passed / 0 failed, 4090 filtered out**. `schema.is_bounded()`
///   included, because on every fixture the bound really IS narrowed and that assertion holds
///   independently of the range check. A SUBSTRING filter is a different question with a
///   different answer — `cargo test -p phase-engine loop_shortcut` sweeps every target and additionally
///   matches `loop_shortcut_activation` / `loop_shortcut_mana_engine`; do not quote one shape's
///   number beside the other's filter.
///   No single-conjunct revert of step (7) flips a row on THIS fixture, and that is a property
///   of the mutation rather than a gap: it widens only the range's UPPER end, and a bound of
///   exactly `MAX_SHORTCUT_CYCLES` means no axis narrowed — which `classify_win_kind` already
///   reports as `Advantage`, so step (5) refuses two conjuncts earlier. The REACHABLE end is
///   the lower one, and its named row is
///   `game::engine::bounded_offer_conjunct_tests::a_bound_of_zero_mints_no_bounded_offer`
///   (revert-probe: `0..MAX_SHORTCUT_CYCLES` ⇒ that row FAILS; measured).
/// * remove `elimination_bounds`' `p.life as i64 - 1` headroom term ⇒ the recomputed bound
///   and the published one diverge ⇒ FAILS.
#[test]
fn dina_untargeted_drain_4p_offers_at_three_live_opponents() {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));

    // ── reach-guards on the loaded board; every assertion below is meaningless without them
    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring, so no offer could ever \
         be raised and this row would be vacuous; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring — every frame the offer certifies \
         against was accumulated by THIS drive, not restored"
    );
    assert_eq!(
        engine_live_opponents(&state, P0).len(),
        3,
        "reach-guard: the whole point of this class is that Path A cannot crown, which needs \
         >= 2 non-fallers, i.e. three living opponents here"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "reach-guard: the dump must NOT ship at a saved offer — the offer is this row's \
         deliverable, not its input"
    );

    let beat = drive_to_bounded_offer(&mut state, 400).expect(
        "CR 732.2a: the bounded offer must FIRE on this real 4p drain. BASE (no Path D) drove \
         326 beats on this same dump and reached zero LoopShortcut beats, so a failure here is \
         the offer never being raised, not a fixture accident.",
    );

    let (proposer, certificate, schema) = bounded_offer_parts(&state);

    // ── the two binding field-value discriminators ──
    assert_eq!(
        proposer, state.active_player,
        "CR 732.2a: the proposer is the priority holder, and step (2) requires that to be the \
         active player the ring sampler gates on"
    );
    assert!(
        state.last_loop_action_sequence.is_empty(),
        "the bounded class's entry must NOT require a driving sequence — a non-empty one \
         routes an accepted proposal to the object-growth materializer, which commits zero \
         bounded cycles (beat {beat})"
    );
    assert!(
        schema.points.is_empty(),
        "the UNTARGETED class publishes no per-iteration choice, so the schema exposes no \
         decision points; got {:?}",
        schema.points
    );

    // ── the per-period signature, bound FROM the value ──
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .expect("a bounded offer publishes the per-period signature its bound was divided by");
    assert!(
        per_cycle.frames_per_period >= 1,
        "a period spans at least one retained frame; got {}",
        per_cycle.frames_per_period
    );
    assert!(
        per_cycle.victim_slot.is_empty(),
        "no slot is published, so nothing is charged to a declared victim — the victims are \
         already visible in `delta.life`; got {:?}",
        per_cycle.victim_slot
    );
    assert!(
        per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
        "a zero-delta cycle states no CR 704 threshold and must never be offered"
    );

    // ── the bound, RECOMPUTED from the offer-beat board ──
    let living_opponents: Vec<PlayerId> = engine_live_opponents(&state, proposer);
    assert_eq!(
        living_opponents.len(),
        3,
        "three living opponents must still be the population AT THE OFFER BEAT ({beat}), not \
         only at load — otherwise the bound below is computed over the wrong seats"
    );
    let mut losses: Vec<(PlayerId, i64, i64)> = vec![];
    for p in state.players.iter().filter(|p| !p.is_eliminated) {
        let loss = -per_cycle.delta.life.get(&p.id).copied().unwrap_or(0);
        losses.push((p.id, p.life as i64, loss));
    }
    let opponent_losses: Vec<i64> = losses
        .iter()
        .filter(|(id, _, _)| *id != proposer)
        .map(|(_, _, loss)| *loss)
        .collect();
    assert!(
        opponent_losses.iter().all(|&l| l > 0),
        "REACH-GUARD against a degenerate fixture: every living opponent must actually be \
         LOSING life per cycle, else the CR 704.5a headroom term never narrows and the bound \
         below would be the safety cap for the wrong reason; measured {losses:?}"
    );
    let expected_bound = losses
        .iter()
        .filter(|(_, _, loss)| *loss > 0)
        .map(|(_, life, loss)| (life - 1) / loss)
        .min()
        .expect("at least one seat is losing life, asserted above");
    assert_eq!(
        i64::from(schema.max_iterations),
        expected_bound,
        "CR 704.5a: the published bound must equal `min over living seats of (life - 1) / \
         per-cycle loss`, recomputed here from the offer-beat board {losses:?} at beat {beat}"
    );
    assert_eq!(
        schema.iteration_count,
        engine::analysis::decision_template::IterationCount::Fixed(schema.max_iterations),
        "CR 732.1b: the SUGGESTION seeded into the picker is the bound itself"
    );
    assert!(
        schema.is_bounded(),
        "the whole claim of this producer is that it NARROWED the repetition bound; \
         max_iterations = {}",
        schema.max_iterations
    );

    // ── siblings: nothing terminal happened, and no revocable-infinity was marked ──
    assert_eq!(
        certificate.win_kind,
        engine::analysis::loop_check::WinKind::LethalDamage,
        "CR 704.5a: a life drain is not `Advantage`, which is the conjunct that keeps this \
         seam disjoint from the Path C revocable-infinity mark"
    );
    assert!(
        state.unbounded_resources.is_empty(),
        "an OFFER is not a grant: CR 104.4b's revocable-infinity mark belongs to Path C and \
         must not be written by raising a bounded offer; got {:?}",
        state.unbounded_resources
    );

    // ── F6: THE PREVIEW REACHES A REAL GAME ──
    //
    // Every other row that asserts anything about the CR 732.2a count preview hand-builds a
    // `WaitingFor` and projects it. That leaves one hole none of them can see: the preview is
    // published only when the offer pairs `per_cycle: Some` with a FINITE count, and the
    // producer that pairs them is the bounded one. Route the bound through
    // `shortcut_iteration_count` — which returns `UntilLethal` for `LethalDamage | PoisonLoss`,
    // and this fixture's `win_kind` is exactly `LethalDamage` — and the preview vanishes from
    // EVERY real game while all three hand-built rows stay green. This row closes that by
    // reading the preview off the real 4p dump the engine itself raised the offer on.
    //
    // WHAT WRONG IMPLEMENTATION WOULD STILL PASS THIS ROW? One that publishes the preview only
    // on this fixture's exact per-period shape (the hand-built rows cover the shape space), and
    // one that previews the right numbers for a count the player did not pick — the reserved
    // per-selected-count question, deliberately not answered here.
    //
    // REVERT-PROBE, RUN: mint this offer's count through `shortcut_iteration_count` (i.e.
    // `UntilLethal` for a lethal drain) ⇒ `preview` is `None` ⇒ the expect below FAILS.
    let suggested = i64::from(schema.max_iterations);
    let life_deltas: Vec<(PlayerId, i64)> = per_cycle
        .delta
        .life
        .iter()
        .filter(|(_, delta)| **delta != 0)
        .map(|(seat, delta)| (*seat, *delta))
        .collect();
    assert!(
        life_deltas.len() >= 2,
        "reach-guard: the preview's per-seat fold only discriminates when the period moves \
         MORE THAN ONE seat's life — a single-seat period is satisfiable by an implementation \
         that keys every entry to the proposer; measured {life_deltas:?}"
    );

    engine::game::interaction::bind_interaction_authority(
        &mut state,
        engine::types::interaction::InteractionSessionId("dina-preview".to_string()),
    )
    .expect("the offer beat binds an interaction authority");
    let filtered = engine::game::visibility::filter_state_for_viewer(&state, proposer);
    let view = engine::game::interaction::derive_viewer_interaction(&state, &filtered, proposer);
    let engine::types::interaction::InteractionOpportunityResponse::Schema {
        spec: engine::types::interaction::InteractionResponseSpec::Shortcut { preview, .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("the bounded offer publishes a shortcut schema to its proposer");
    };
    let preview = preview.as_ref().expect(
        "CR 732.2a: the offer the engine raised on a REAL 4p drain must publish what its \
         declared count does. A `None` here means every preview has vanished from every real \
         game while the hand-built projection rows stayed green.",
    );
    assert_eq!(
        i64::from(preview.count),
        suggested,
        "the magnitudes are stated for the offer's own suggested count and no other"
    );

    let mut expected: Vec<(Option<u8>, i32)> = life_deltas
        .iter()
        .map(|(seat, delta)| (Some(seat.0), (delta * suggested) as i32))
        .collect();
    let mut published: Vec<(Option<u8>, i32)> = preview
        .entries
        .iter()
        .filter(|entry| {
            entry.family == engine::types::interaction::InteractionShortcutPreviewFamily::Life
        })
        .map(|entry| (entry.player, entry.amount))
        .collect();
    expected.sort_unstable();
    published.sort_unstable();
    assert_eq!(
        published, expected,
        "CR 119.3: every seat the certified period moves life on is previewed at that seat, \
         multiplied out by the declared count — recomputed here from the offer-beat certificate"
    );
    for (seat, _, loss) in losses.iter().filter(|(id, _, _)| *id != proposer) {
        assert!(
            published.contains(&(Some(seat.0), (-loss * suggested) as i32)),
            "CR 704.5a: victim seat {seat:?} loses {loss} per cycle, so its previewed life \
             entry must be the NEGATIVE finished magnitude on that seat's own key — a \
             proposer-keyed subject map publishes it on the wrong HUD; got {published:?}"
        );
    }
}

/// The dina 4p drain DRIVEN through `apply()` to the beat the engine itself raises the
/// bounded offer on, with that beat's index. Shared by every row that has to observe the
/// mint at the ONE beat this corpus offers on.
fn dina_driven_to_bounded_offer() -> (GameState, usize) {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    let beat = drive_to_bounded_offer(&mut state, 400).expect(
        "CR 732.2a: the bounded offer must FIRE on this real 4p drain. BASE (no Path D) drove \
         326 beats on this same dump and reached zero LoopShortcut beats, so a failure here is \
         the offer never being raised, not a fixture accident.",
    );
    (state, beat)
}

/// Restore the `Priority` window the bridge consumed when it raised the offer, so the mint can
/// be re-run on the offer beat's own board. Everything else the mint reads — the ring, the
/// stack, the resources, `last_loop_action_sequence` — is untouched, and each caller proves
/// the reconstruction faithful by requiring the SAME outcome the production path produced.
fn replay_at_priority(state: &GameState, proposer: PlayerId) -> GameState {
    let mut replay = state.clone();
    replay.waiting_for = WaitingFor::Priority { player: proposer };
    replay
}

/// R16 (v) — THE SEQUENCING PIN: NOTHING SPENDS BEFORE THE RING GATE, BECAUSE NOTHING ASKS.
///
/// CR 732.2a. At a ring-WARM-UP beat (`loop_detect_ring.len() < 2`) there is no window, hence
/// no reachable certificate, so the mint must refuse at the ring-usability gate BEFORE the
/// verdict door is asked anything. This is the property that keeps the frozen exemption's
/// cost argument honest: at these beats the frozen set is empty and dellian's non-exempt
/// population is 152–153 entries, i.e. exactly the unexempted full sweep the ring gate exists
/// to keep off the critical path.
///
/// The property is STRUCTURAL, not an ordering the executor had to remember: the verdict
/// container is constructed BELOW the gate, so at this beat there is nothing to ask.
///
/// ⚠ WHAT THIS ROW DOES **NOT** CATCH, stated because the plan's proposed revert-probe was
/// analysed NOT to flip through this instrument. `MintMeter` is populated from the container
/// AFTER `certified_bounded_cycle_offer` returns, and the ring gate is an EARLY RETURN above
/// that snapshot — so an eager pass hoisted above the gate would spend a budget this meter
/// never reads, and the all-zero reading below would survive it. The row therefore pins the
/// OBSERVABLE part (the gate refuses with `NoCertification`, on a beat carrying a stack an
/// eager pass would really pay for) while the structural part rests on the construction order
/// in `bounded_cycle_offer`. Disclosed as a stop-and-return item rather than papered over with
/// a probe that cannot fire.
#[test]
fn r16v_a_ring_warmup_beat_spends_nothing() {
    use engine::game::engine::{
        try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
    };

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    )));
    // Find a beat that is AT PRIORITY (so steps 1/1b/2 pass and the ring gate is the first
    // thing that can refuse) and still ring-starved. The dump ships with an empty ring, so
    // this is reachable by construction; the search makes the row robust to drive drift.
    let mut found = None;
    for beat in 0..40usize {
        if state.loop_detect_ring.len() < 2 {
            if let WaitingFor::Priority { player } = state.waiting_for {
                if player == state.active_player && state.last_loop_action_sequence.is_empty() {
                    found = Some((beat, state.clone()));
                    break;
                }
            }
        }
        if dump_drive_one_beat(&mut state, None).is_err() {
            break;
        }
    }
    let (beat, board) = found.expect(
        "REACH-GUARD: no ring-starved beat at priority was reached, so this row would be \
         asserting about a gate it never arrived at",
    );
    assert!(
        board.stack.len() > 2,
        "REACH-GUARD: the beat must carry a stack an eager pass would actually PAY for, else \
         `spent == 0` is true for want of anything to classify; got {} entries at beat {beat}",
        board.stack.len()
    );

    let (outcome, meter) =
        try_offer_bounded_cycle_shortcut_metered(&board, false, ProbeCap::Shipped);
    assert!(
        matches!(outcome, Err(BoundedOfferRefusal::NoCertification)),
        "a ring of {} frames reaches no window, so the refusal is the ring-usability gate's; \
         got {outcome:?}",
        board.loop_detect_ring.len()
    );
    assert_eq!(
        (
            meter.spent,
            meter.denied,
            meter.conjunct6_asks,
            meter.conjunct4_scans
        ),
        (0, false, 0, 0),
        "R16(v): the gate refuses BEFORE the verdict container can be asked anything — a \
         non-zero counter here is an eager classification pass reintroduced above the ring \
         gate. beat {beat}, meter {meter:?}"
    );
    assert!(
        meter.certification.is_none(),
        "no certificate is reachable at a ring-starved beat; meter {meter:?}"
    );
}

/// R15 — A BUDGET-EXCEEDED MINT IS A REFUSAL, NEVER A STALL AND NEVER A CERTIFICATE.
///
/// CR 732.2a. This is the row that makes *"cost is a coverage knob, never a soundness knob"* a
/// measurement instead of a sentence: with the per-mint cap forced to zero, the classifier can
/// afford nothing, `probe_resolution` returns `Prompted`, and the offer is REFUSED — the mint
/// does not fall through to a clone-and-resolve, and it does not hang.
///
/// The two arms are the SAME BOARD one argument apart — the real dina offer beat, replayed
/// through the only cap channel the seam admits. The `Shipped` arm is the matched positive
/// reach-guard: without it a starved refusal proves nothing, because a board that never offers
/// refuses at zero budget too.
///
/// REVERT-PROBE: delete `probe_resolution`'s `try_charge_one` arm (`resolution_prompt.rs`) ⇒
/// the exhausted budget falls through to the clone-and-resolve ⇒ the starved arm OFFERS ⇒
/// FLIPS.
#[test]
fn r15_a_zero_probe_budget_refuses_the_bounded_offer() {
    use engine::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};

    let (state, beat) = dina_driven_to_bounded_offer();
    let (proposer, _, _) = bounded_offer_parts(&state);
    let replay = replay_at_priority(&state, proposer);

    // MATCHED POSITIVE, first so a starved refusal below can never pass vacuously.
    let (healthy, healthy_meter) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Shipped);
    assert!(
        healthy.is_ok(),
        "REACH-GUARD: at the shipped cap this beat must OFFER, else the starved arm is not \
         keyed to the budget. beat {beat}, meter {healthy_meter:?}"
    );
    assert!(
        !healthy_meter.denied,
        "REACH-GUARD: the positive arm must not itself be exhausted; meter {healthy_meter:?}"
    );

    // THE ROW: the same board, the same beat, the cap forced to zero.
    let (starved, meter) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Lowered(0));
    assert!(
        starved.is_err(),
        "CR 732.2a: an unaffordable probe degrades to honest-red — no certificate and no \
         offer. Got {starved:?}, meter {meter:?}"
    );
    assert!(
        meter.denied && meter.spent == 0,
        "the refusal must be the BUDGET's: a zero cap denies the very first charge, so \
         nothing is spent and the denial flag is what carries the cause. meter {meter:?}"
    );
    // WHERE the denial lands, MEASURED rather than assumed — and it is not the intuitive
    // answer. Basis B consults NO board predicate, so it certifies for FREE even at a zero cap
    // (`certification == Some(ResourceSignatureOnly)` at `spent == 0`); the first gate that
    // must actually PAY is conjunct (6), which asks the door, is denied, and therefore reads
    // `Prompted`. Exhaustion surfaces as an UNSPECIFIED WINDOW, not as a missing certificate.
    assert!(
        meter.conjunct6_asks > 0,
        "REACH-GUARD: the starved mint must have REACHED the paying gate, else `denied` is \
         about a charge nobody ever attempted. meter {meter:?}"
    );
    assert!(
        matches!(
            starved,
            Err(engine::game::engine::BoundedOfferRefusal::UnspecifiedChoiceWindow)
        ),
        "an exhausted classifier reads `Prompted`, so the step-6 predicate goes false and the \
         mint REFUSES — never a stall, never a certified offer. Got {starved:?}, \
         meter {meter:?}"
    );
}

/// R33 arm (d) — THE CORPUS'S ONE OFFERING BEAT CERTIFIES THROUGH BASIS B, AND THE FROZEN
/// EXEMPTION IS THEREFORE WITHDRAWN THERE.
///
/// CR 732.2a. The row R33 (a)/(b)/(a′) prove at the constructor and the selection site; this
/// arm proves it on the REAL board the engine actually offers on, because the plan's cost
/// argument (the frozen subtraction, and the speed-up that rests on it) was measured at
/// *dellian-shaped* beats and this is the beat the corpus *offers* at. Escalated to the lead
/// and ruled on before this test was written; the figures are in the commit message.
///
/// MEASUREMENT, not re-assertion of a design: the certifying disjunct has no other surface.
/// Both bases publish `frames_per_period`, so `LoopCertificate` discriminates in NEITHER
/// direction — hence [`MintMeter::certification`].
///
/// FIDELITY OF THE OBSERVATION, which is this row's real risk. The production mint runs
/// INSIDE the offering beat's `apply()`, from a `Priority` window the bridge has already
/// consumed by the time the drive returns, so the beat's own meter is unreachable from a
/// test. The mint is re-run here on the offer beat's state with that window restored, and
/// the reconstruction is PROVEN faithful rather than assumed: it must (1) offer at all and
/// (2) publish a `per_cycle` EQUAL to the one the production path wrote into `waiting_for`.
/// A reconstruction that drifted would fail (1) or (2) before the basis assertion is reached.
#[test]
fn dina_offering_beat_certifies_through_basis_b_and_exempts_nothing() {
    use engine::analysis::resource::PeriodCertification;
    use engine::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};

    let (state, beat) = dina_driven_to_bounded_offer();
    let (proposer, certificate, _) = bounded_offer_parts(&state);
    let published = certificate
        .per_cycle
        .clone()
        .expect("the bounded offer publishes a per-cycle signature");

    let replay = replay_at_priority(&state, proposer);
    let (outcome, meter) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Shipped);

    // (1) reconstruction fidelity, part one
    assert!(
        outcome.is_ok(),
        "REACH-GUARD: the replayed mint must reach the same OFFER the production path raised \
         at beat {beat}; a refusal here means this row measures a different board than the \
         engine did, and the basis assertion below would be about nothing. Got {outcome:?}, \
         meter {meter:?}"
    );
    // (1) reconstruction fidelity, part two — the same certificate, not merely some offer
    let WaitingFor::LoopShortcut {
        certificate: replayed,
        ..
    } = outcome.expect("asserted Ok above")
    else {
        panic!("the bounded offer is a LoopShortcut window");
    };
    assert_eq!(
        replayed.per_cycle.as_ref(),
        Some(&published),
        "REACH-GUARD: the replayed mint must publish the SAME per-cycle signature as the \
         production offer at beat {beat}"
    );

    // (2) THE MEASURED AXIS. Basis A certified NOTHING corpus-wide (0 of 129 certifications
    // across the three 4p dumps); this beat takes `ring_delta_signature`, which by its own
    // doc consults no board predicate.
    assert_eq!(
        meter.certification,
        Some(PeriodCertification::ResourceSignatureOnly),
        "R33: the corpus's one offering beat (beat {beat}) certifies through BASIS B. If this \
         ever reads `BoardCovered`, the frozen exemption became available at an offering beat \
         and the plan's exempted-cost row must be re-derived before that is relied on"
    );

    // (3) THE CONSEQUENCE, which is the half that makes this arm about the exemption rather
    // than about a label: under a non-`BoardCovered` certificate `frozen_ids` is empty, so
    // conjunct (6) skips NOTHING and scans every non-exempt entry it is handed.
    assert_eq!(
        meter.conjunct6_frozen_skips, 0,
        "R33: `ResourceSignatureOnly` supplies neither P2 nor P4, so the subtraction is \
         withdrawn and conjunct (6) exempts nothing at this beat"
    );
    assert!(
        meter.conjunct6_asks > 0,
        "REACH-GUARD against a vacuous skip count: conjunct (6) must actually have RUN at \
         this beat, else `frozen_skips == 0` is trivially true; meter {meter:?}"
    );

    // (4) THE BUDGET, re-derived from this very beat (R16(ii-a)). The offer fires WITH the
    // cap binding, not because the cap stopped mattering.
    assert!(
        !meter.denied,
        "R16(ii-a): the shipped cap must not starve the corpus's acceptance offer; measured \
         demand at this beat is 13 charges. meter {meter:?}"
    );
}

/// R16 (i) + (ii-a) + (iv) — THE BUDGET DOES NOT STARVE THE CORPUS'S ACCEPTANCE BEAT, ITS
/// DEMAND THERE IS EXACTLY MEASURED, AND THE MINT DOES NOT STALL.
///
/// CR 732.2a. Without this row the per-mint probe cap is a number nobody checked against the
/// fixture it has to serve — which is exactly how the shipped `12` came to starve this beat by
/// one charge.
///
/// (i) the offer still fires at the shipped cap. (ii-a) the cap does NOT bind there
/// (`denied == false`) — note that `spent <= cap` is true by construction of the budget, so
/// the non-vacuous form of (ii-a) is the denial flag, not the inequality.
///
/// THE EXACT-DEMAND PIN is what makes (ii-a) discriminating rather than a restatement. The
/// demand `D` is SEARCHED through the seam's own closed cap domain, from zero upward, so it
/// is measured on this run instead of copied from a log: every cap below `D` must REFUSE and
/// `D` itself must OFFER. A budget re-derivation that drifts the true demand fails here with
/// the new number in the message.
///
/// (iv) THE WALL-CLOCK. The whole mint — memo construction, the D2 window work
/// (`certified_period_touch` + `bounded_cycle_pin_slots_for_window` over up to
/// `LOOP_DETECT_RING_CAP` windows) and the budgeted classification — is timed end to end.
/// ⚠ THE CEILING IS DEBUG-SCALED AND SAYS SO: the plan's ~1 s figure is a player-facing
/// RELEASE budget, and this binary is `-C opt-level=0`. The measured figure is carried in the
/// message so the release claim is derived from a number rather than asserted.
///
/// REVERT-PROBE: lower `PROBE_BUDGET` below the measured `D` ⇒ (i) fails
/// (`dina_driven_to_bounded_offer` cannot reach an offer) ⇒ FLIPS. Round 2 measured exactly
/// that: at `12` this row and six shipped siblings go red together.
#[test]
fn r16_the_offering_beats_probe_demand_is_exactly_measured() {
    use engine::game::engine::{
        try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
    };

    let (state, beat) = dina_driven_to_bounded_offer();
    let (proposer, _, _) = bounded_offer_parts(&state);
    let replay = replay_at_priority(&state, proposer);

    // ── (i) + (iv): the shipped cap, timed ───────────────────────────────────────────────
    let started = std::time::Instant::now();
    let (shipped, meter) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Shipped);
    let elapsed = started.elapsed();
    assert!(
        shipped.is_ok(),
        "R16(i) beat {beat}: the corpus's acceptance offer must FIRE at the shipped cap. \
         Got {shipped:?}, meter {meter:?}"
    );
    assert!(
        !meter.denied,
        "R16(ii-a) beat {beat}: the cap must not BIND at the beat that offers — `spent <= cap` \
         is true of every mint by construction, so the denial flag is the only non-vacuous \
         form of this claim. meter {meter:?}"
    );
    assert!(
        meter.spent > 0,
        "REACH-GUARD: the mint must have CLASSIFIED something, else `!denied` is true for \
         want of anything to charge. meter {meter:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "R16(iv) beat {beat}: the whole mint took {elapsed:?}, which is past even the \
         debug-scaled stall ceiling. Measured at the time of writing: ~12 ms for this beat's \
         `ring=3 stack=10` mint in an unoptimized build. meter {meter:?}"
    );

    // ── THE EXACT-DEMAND PIN ─────────────────────────────────────────────────────────────
    let demand = meter.spent;
    let (at_demand, demand_meter) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Lowered(demand));
    assert!(
        at_demand.is_ok() && !demand_meter.denied,
        "R16: a cap of exactly the measured demand ({demand}) must still OFFER — that is what \
         makes {demand} the DEMAND rather than an upper bound. Got {at_demand:?}, \
         meter {demand_meter:?}"
    );
    for lowered in 0..demand {
        let (starved, starved_meter) =
            try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Lowered(lowered));
        assert!(
            matches!(starved, Err(BoundedOfferRefusal::UnspecifiedChoiceWindow)),
            "R16: every cap below the measured demand must refuse fail-CLOSED at the choice \
             gate. cap {lowered} of {demand} gave {starved:?}, meter {starved_meter:?}"
        );
        assert!(
            starved_meter.denied,
            "R16: …and the refusal must be attributable to the BUDGET, not to an unrelated \
             conjunct. cap {lowered}, meter {starved_meter:?}"
        );
    }
    assert!(
        demand > 1,
        "REACH-GUARD: a demand of 0 or 1 would make the starvation sweep above empty or \
         trivial; measured {demand} at beat {beat}"
    );
}

/// Drive a `GameScenario`-built board until the ENGINE writes a bounded offer, declining
/// nothing and injecting nothing. Returns the beat, or `None` if the cap ran out. Reads
/// `state.waiting_for` — the production Path D write — never an out-of-band predicate call.
fn drive_scenario_to_bounded_offer(runner: &mut GameRunner, cap: usize) -> Option<usize> {
    for beat in 0..cap {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::LoopShortcut {
                predicted_winner: None,
                ..
            }
        ) {
            return Some(beat);
        }
        if dump_drive_one_beat(runner.state_mut(), None).is_err() {
            return None;
        }
    }
    None
}

/// PR-7 Phase 5b — THE BASIS-B POSITIVE CONTROL, at the offer level.
///
/// This is the row whose WHOLE PURPOSE is detecting a back-door deletion of certification
/// basis B. It is **not** the only row that detects one — an earlier revision of this doc said
/// it was ("every other bounded-offer row in this file certifies on basis A") and that was
/// MEASURABLY FALSE, and it licenses exactly the overread that the ⚠ SCOPE note on
/// [`basis_a_bounded_fixed_count_commits_exactly_n_periods`] exists to prevent — which a reader
/// reaches much later in this file than this sentence.
///
/// RE-MEASURED in fix round 4 at `025015135`, using this file's own prescribed attribution
/// probe (force `ring_delta_signature` to return `None` unconditionally — basis-B rows lose
/// their offer, basis-A rows keep it), runner
/// `cargo test -p phase-engine --test integration -- loop_shortcut::` (module filter on the
/// `integration` binary): **74 passed / 11 failed / 4090 filtered out**, against a clean
/// **85 passed / 0 failed / 4090 filtered out**. ELEVEN rows flip. This one, plus these ten:
/// `dina_untargeted_drain_4p_offers_at_three_live_opponents`,
/// `bloodloop_mandatory_draw_cascade_offers_at_2p_3p_and_4p`,
/// `ai_bounded_declare_candidate_is_generated_legal_and_drives`,
/// `bounded_fixed_count_commits_exactly_n_periods`,
/// `bounded_fixed_drive_stops_at_the_first_lethal_cycle`,
/// `bounded_fixed_drive_rolls_back_a_partial_crossing_cycle`,
/// `a_cycle_that_does_not_match_the_published_period_is_dropped`,
/// `declared_count_above_the_offered_bound_is_handed_back`,
/// `until_lethal_against_a_bounded_offer_is_rejected`,
/// `a_proposers_own_driving_period_mints_no_bounded_offer`.
///
/// FIXTURE PROVENANCE of those eleven, RE-COUNTED in fix round 5 over the whole set rather
/// than asserted of one row (an earlier revision annotated dina alone as "the real 4p dump",
/// which reads as an exclusivity it does not have). SIX load the real `dina_conqueror_4p`
/// 4-player capture from `tests/fixtures`, through the same gunzip → restore loader:
/// `dina_untargeted_drain_4p_offers_at_three_live_opponents`,
/// `a_proposers_own_driving_period_mints_no_bounded_offer`,
/// `declared_count_above_the_offered_bound_is_handed_back`,
/// `until_lethal_against_a_bounded_offer_is_rejected`,
/// `bounded_fixed_count_commits_exactly_n_periods` (which loops that dump AND two
/// `bloodloop_state` boards, so it is the one MIXED row), and
/// `bounded_fixed_drive_rolls_back_a_partial_crossing_cycle`. The remaining five are
/// `GameScenario` builds only — this row inline, the other four via `bloodloop_state`. Counted
/// by resolving every fixture-loading call site in this file to its enclosing test fn, NOT by
/// grep hit count: these very sentences add doc-comment hits for the names they list.
/// Consistent with the plural "the file's real 4p dumps" at the ⚠ SCOPE note on
/// [`basis_a_bounded_fixed_count_commits_exactly_n_periods`].
///
/// What is distinctive here is INTENT and DIAGNOSIS, not exclusivity: NINE of those ten fail
/// for a reason their own docs do not name, whereas this row says so in the failure text
/// ("A failure here means basis B minted nothing"). The tenth,
/// `dina_untargeted_drain_4p_offers_at_three_live_opponents`, is the EXCEPTION and was
/// mis-covered by an earlier "each of those ten" — its ⚠ CERTIFICATION BASIS note documents
/// this very probe as measurement (ii), down to the drive running its full 400-beat cap and
/// its `expect` firing. Checked against all ten doc blocks in fix round 5: no other one
/// mentions `ring_delta_signature` or basis B at all. Nor is the list a basis census: it is the
/// set of rows that cannot reach their assertions once basis B stops minting, which includes
/// rows whose subject is the DRIVE rather than the basis.
///
/// FIXTURE — a fully mandatory two-card draw↔drain cascade. What it shares with the class
/// basis B exists for is the one property that matters: **it draws a card every cycle**, so a
/// card moves library→hand each period, the board never recurs, and basis A's
/// `loop_states_equal_modulo_resources` (library and hand are board, not projected resources)
/// and its cover disjunct must BOTH refuse. The `None =>` arm is then the only way an offer
/// can be minted here.
///
/// ORACLE-TEXT PROVENANCE, verified against the Scryfall API, and deliberately honest:
/// * *"Whenever you draw a card, each opponent loses 1 life."* is **real** — Psychosis
///   Crawler's second ability, verbatim.
/// * *"Whenever an opponent loses life, draw a card."* matches **no printing**. The two
///   nearest real cards are both GATED — Kefka, Ruler of Ruin (*"…during your turn"*) and
///   Valgavoth, Harrower of Souls (*"…for the first time during each of their turns"*) — and
///   that gating is precisely what would stop the cascade. This fixture is therefore
///   SYNTHETIC and deliberately stronger than any printed card. It must not be described as a
///   real-card loop. Its matched negative control
///   (`drawgo_ring_spans_turns_but_never_offers`) is a `GameScenario` too, which is the
///   precedent for a synthetic matched control pair for one predicate.
///
/// MEASURED at the seam (not from an out-of-band predicate call): the engine writes the offer
/// at beat 31, turn 4, `Draw`, with a derived `frames_per_period == 2`, δ =
/// `life{P1:-1} lib{P0:-1}` and a bound of 16; the whole ring sits at `turns[4×6]`
/// `phases[Draw×6]` `extra[0×6]`, so every consecutive pair is turn-position invariant and the
/// CR 703.1 conjunct passes. Note δ carries `lib{P0:-1}` ONLY — P1's library is untouched, so
/// no second draw step is inside the period. Contrast drawgo (`lib{P0:-1,P1:-1}`), whose
/// "period" is one 2-player turn cycle.
///
/// REVERT-PROBE ⓐ (MEASURED, not predicted): make `ring_delta_signature` return `None`
/// unconditionally ⇒ the `None =>` arm converts that into `Err(NoCertification)` ⇒ no offer is
/// written ⇒ the `expect` below FAILS.
///
/// ⚠ SCOPE, stated so it is not overread: this SYNTHETIC control fires at TWO players. The
/// same cascade built at 3 and 4 players certifies but mints zero offers (they refuse
/// downstream, at step (6) `stack_choices_are_all_specified`, because `Effect::Draw` is
/// outside that gate's allow-list). `multiplayer_pure_life_drain_offers_at_three_and_four_players`
/// is NOT a substitute — it is measured basis A. The ≥3-player basis-B coverage this file DOES
/// carry is `dina_untargeted_drain_4p_offers_at_three_live_opponents`, which is measured basis
/// B (k == 1) on a real 4p dump; see that row's doc for the measurement and for why its
/// published payload cannot assert the basis on its own.
#[test]
fn bounded_offer_on_a_within_turn_draw_drain_is_basis_b() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(
        P0,
        "Test Bleeder",
        2,
        2,
        "Whenever you draw a card, each opponent loses 1 life.",
    );
    scenario.add_creature_from_oracle(
        P0,
        "Test Chronicler",
        2,
        2,
        "Whenever an opponent loses life, draw a card.",
    );
    // CR 504.1: the libraries must outlast the drive — a deck-out would end the game and
    // silently truncate every assertion below.
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    scenario.with_library_top(P0, &refs);
    scenario.with_library_top(P1, &refs);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    assert!(
        runner.state().loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring, so no offer could ever be \
         raised and this row would be vacuous"
    );
    assert_eq!(
        runner.state().loop_detect_ring.len(),
        0,
        "reach-guard: every frame the offer certifies against is accumulated by THIS drive"
    );

    let beat = drive_scenario_to_bounded_offer(&mut runner, 200).expect(
        "CR 732.2a: the within-turn mandatory draw↔drain cascade must raise a bounded offer. \
         A failure here means basis B minted nothing — which is exactly what revert-probe ⓐ \
         (make `ring_delta_signature` return None) produces.",
    );

    let state = runner.state();
    let (proposer, certificate, schema) = bounded_offer_parts(state);

    // (i) the binding field-value discriminator.
    assert_eq!(
        proposer, state.active_player,
        "CR 732.2a: the proposer is the priority holder, and step (2) requires that to be the \
         active player the ring sampler gates on"
    );

    // (ii) the period, bound FROM the returned value — no literal `2` appears in this row.
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .expect("a bounded offer publishes the per-period signature its bound was divided by");
    let k = per_cycle.frames_per_period;
    assert!(
        k >= 1,
        "a period spans at least one retained frame; got {k}"
    );
    // Bound to a local rather than inlined: `2k + 1` is the CONTRACT expression (2k deltas ⇒
    // the period was observed twice), and clippy's `int_plus_one` would otherwise push it to a
    // `> 2k` that no longer reads as the rule.
    let frames_needed = 2 * k as usize + 1;
    assert!(
        state.loop_detect_ring.len() >= frames_needed,
        "the structural invariant every certified period satisfies: a period seen TWICE needs \
         2k+1 frames. k = {k}, ring = {} (beat {beat})",
        state.loop_detect_ring.len()
    );
    assert!(
        per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
        "a zero-delta cycle states no CR 704 threshold and must never be offered"
    );

    // (iii) A PERIOD-WIDTH TRIPWIRE — no longer a basis attribution (fix round 2). The older
    // wording read `!= 1` as sufficient for "not basis A" on the premise that basis A published
    // a hardcoded `1`. Fix round 1 replaced that hardcode with a MEASURED span, so basis A now
    // publishes 2 on at least one shipped fixture
    // (`interactive_3p_subset_lethal_does_not_crown`) and the inference is dead in both
    // directions. The basis attribution this row actually stands on is its named revert-probe ⓐ
    // — force `ring_delta_signature` to return `None` and this row loses its offer entirely,
    // which no basis-A row does. The assertion is KEPT as a width tripwire: this cascade draws a
    // card every cycle, so its period genuinely spans more than one retained frame, and a drift
    // to 1 means the fixture stopped being what the row describes.
    assert_ne!(
        k, 1,
        "this cascade draws a card every cycle, so one repetition spans more than one retained \
         ring frame; a k of 1 means the fixture no longer has the shape this row asserts"
    );

    // (iv) the bound is narrowed, asked of the single authority. `MAX_SHORTCUT_CYCLES` is
    // `pub(crate)` and unnameable from an integration test; `is_bounded()` is the shipped
    // `pub` predicate for exactly this question.
    assert!(
        schema.max_iterations >= 1,
        "a bound of 0 states no repetition and must not be offered"
    );
    assert!(
        schema.is_bounded(),
        "the whole claim of this producer is that it NARROWED the repetition bound below the \
         engine-wide safety cap; max_iterations = {}",
        schema.max_iterations
    );

    // (v) the untargeted class publishes no per-iteration choice.
    assert!(
        schema.points.is_empty(),
        "the UNTARGETED class exposes no decision points; got {:?}",
        schema.points
    );
}

/// PR-7 Phase 5b — the MULTIPLAYER offer control: the untargeted every-opponent drain raises a
/// bounded offer at THREE and at FOUR players, not only at two.
///
/// FIXTURE — the pure life↔life cascade, both halves verbatim real-card Oracle text verified
/// against the Scryfall API: Marauding Blight-Priest (*"Whenever you gain life, each opponent
/// loses 1 life."*) plus Exquisite Blood (*"Whenever an opponent loses life, you gain that much
/// life."*). Untargeted and every-opponent, which is the true full-multiplayer drain and the
/// same class as the real 4p `dina_conqueror_4p` dump.
///
/// WHY THIS SHAPE: its stack holds only `GainLife` / `LoseLife`, so it clears step (6)
/// `stack_choices_are_all_specified` and is DECOUPLED from the separate `Effect::Draw`
/// allow-list hole that silences the 2-player basis-B control at ≥3 players. A control that
/// steered around that hole would guard nothing about it; this one does not touch it.
///
/// UNEQUAL OPPONENT LIFE IS LOAD-BEARING, not decoration. With every opponent falling, the
/// living partition is `nonfallers == {P0}`, and at EQUAL life `live_mandatory_loop_winner`'s
/// CR 704.3 simultaneity floor passes and Path A crowns P0 while the kick-off is still
/// resolving — measured, at 2, 3 and 4 players. Staggering the totals fails
/// `fallers_lives_pairwise_equal`, which is what leaves the board in the "lethal to some,
/// crowns nobody" regime the bounded offer exists to serve.
///
/// ⚠ BASIS: **A** — established by DISCRIMINATING PROBE, never from `frames_per_period`.
/// The probe: force `ring_delta_signature` to return `None` unconditionally (basis B's only
/// entry point, reached solely from the `basis_a` match's `None =>` arm). This row stays
/// GREEN at both player counts while `dina_untargeted_drain_4p_offers_at_three_live_opponents`
/// and `bounded_offer_on_a_within_turn_draw_drain_is_basis_b` both FAIL. Surviving that
/// mutation is what proves basis A certified here. **`frames_per_period == 1` proves NOTHING
/// about the basis** — basis B *derives* `k` from `1` upward, so a k==1 basis-B offer is
/// byte-identical in the payload, and since fix round 1 basis A MEASURES its span too (2 on
/// `interactive_3p_subset_lethal_does_not_crown`), so a `!= 1` reading is dead as well. The
/// `== 1` assertion below is a structural consistency check on THIS fixture's period width,
/// necessary but never sufficient; treating it as a basis attribution is the exact
/// non-discriminating inference that mislabelled the dina row.
///
/// ⚠ MECHANISM — CORRECTED, and the correction is load-bearing. An earlier revision of this
/// doc said *"a pure life↔life loop moves only axes `loop_states_equal_modulo_resources`
/// projects out, so the board DOES recur and basis A always matches."* That is MEASURABLY
/// FALSE on this very fixture: the loop moves the STACK, which is not a projected axis.
/// Instrumenting the `basis_a` walk at the offer beat (ring length 3, walked newest-first):
/// * `ring[2]` — the only pair with `eq == true` (stack unchanged) — carries a ZERO δ, so
///   `net_progress_for(proposer)` is **false** and the pair is discarded.
/// * `ring[1]` — `eq == FALSE`, and this is the pair that certifies, through the
///   **`loop_states_cover_modulo_growth_pinned` disjunct**, never through the equal one.
///   The stack grows one period's worth of `Test Exquisite Blood` triggered abilities:
///   **3p `stack[2 -> 3]` (+1), 4p `stack[3 -> 5]` (+2)**. Cover exists for exactly this —
///   growth confined to places `prior` already occupied, by mandatory no-ordering-input
///   triggers.
///
/// So the real reason basis A wins here is not resource-purity: it is that nothing on this
/// board trips a cover gate. Contrast dina, whose identical-in-kind stack growth also clears
/// cover's gates (1)-(4) and is then vetoed at **gate (5)** by an off-stack `ModifyCost`
/// static whose fire-time condition reads a projected axis. The shared write-up lives at the
/// `basis_a` dispatch site in `game::engine::try_offer_bounded_cycle_shortcut`.
///
/// ⚠ CONSEQUENCE for "no basis-B control at ≥3 players from this shape": still true AS BUILT,
/// but for the corrected reason — cover SUCCEEDS here, so the `None =>` arm is never reached.
/// That is a property of this board, not a resource-purity invariant: adding a gate-(5)
/// refuser to the same two cards would flip it to basis B. The ≥3p basis-B coverage this file
/// carries is `dina_untargeted_drain_4p_offers_at_three_live_opponents` (basis B, k == 1, on a
/// real 4p dump).
///
/// REVERT-PROBE (must FLIP): delete the Path D block in `interactive_loop_bridge` ⇒ no offer
/// at either player count ⇒ the `expect` FAILS.
#[test]
fn multiplayer_pure_life_drain_offers_at_three_and_four_players() {
    /// Marauding Blight-Priest, verbatim (Scryfall).
    const BLIGHT_PRIEST: &str = "Whenever you gain life, each opponent loses 1 life.";
    /// Exquisite Blood, verbatim (Scryfall).
    const EXQUISITE_BLOOD: &str = "Whenever an opponent loses life, you gain that much life.";

    /// Build the cascade at `seats` players with staggered opponent life, cast the kick-off,
    /// and return the runner plus the seats the engine considers living opponents of P0.
    fn cascade(seats: u8) -> GameRunner {
        let mut scenario = GameScenario::new_n_player(seats, 7);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        for (i, seat) in (1..seats).map(PlayerId).enumerate() {
            // Staggered: pairwise-UNEQUAL absolute life, equal per-cycle delta.
            scenario.with_life(seat, 1000 + 50 * i as i32);
        }
        scenario.add_creature_from_oracle(P0, "Test Blight Priest", 2, 2, BLIGHT_PRIEST);
        scenario.add_creature_from_oracle(P0, "Test Exquisite Blood", 2, 2, EXQUISITE_BLOOD);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let _ = runner.cast(kickoff).resolve();
        runner
    }

    for seats in [3u8, 4] {
        let mut runner = cascade(seats);
        let opponents = engine_live_opponents(runner.state(), P0);
        assert_eq!(
            opponents.len(),
            usize::from(seats) - 1,
            "reach-guard at {seats} players: every opponent must still be living at the offer \
             beat, else the bound below is computed over the wrong seats"
        );
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
            "reach-guard at {seats} players: the staggered life totals must keep Path A from \
             crowning while the kick-off resolves — at EQUAL life it does, and then there is \
             no board left to offer on; got {:?}",
            runner.state().waiting_for
        );

        let beat = drive_scenario_to_bounded_offer(&mut runner, 200).unwrap_or_else(|| {
            panic!(
                "CR 732.2a: the untargeted every-opponent drain must raise a bounded offer at \
                 {seats} players. This is the multiplayer half of the claim — a 2-player-only \
                 detector is not what this lane ships."
            )
        });

        let state = runner.state();
        let (proposer, certificate, schema) = bounded_offer_parts(state);
        assert_eq!(
            proposer, state.active_player,
            "{seats}p: CR 732.2a step (2) — the proposer is the active priority holder"
        );
        assert!(
            schema.points.is_empty(),
            "{seats}p: the UNTARGETED class exposes no decision points; got {:?}",
            schema.points
        );

        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .expect("a bounded offer publishes its per-period signature");
        assert_eq!(
            per_cycle.frames_per_period, 2,
            "{seats}p: a PERIOD-WIDTH tripwire, not a basis attribution. This cascade puts its \
             two same-controller triggers on the stack through a CR 603.3b `OrderTriggers` \
             window, and the answer-beat sampling site retains a frame there as well as at the \
             settle — so ONE repetition now spans 2 retained frames. A drift \
             means the fixture changed shape and the row must be re-derived, not relaxed. It \
             establishes nothing about the basis in either direction: basis B derives k from 1 \
             upward, and since fix round 1 basis A measures its span too (2 on \
             `interactive_3p_subset_lethal_does_not_crown`). The label is carried by the \
             `ring_delta_signature -> None` probe named in this row's doc, not by this number"
        );

        // EVERY living opponent loses life every cycle — the multiplayer content of the claim.
        // A 2-player-shaped detector that only ever charges one seat fails here.
        let losses: Vec<(PlayerId, i64)> = opponents
            .iter()
            .map(|p| (*p, -per_cycle.delta.life.get(p).copied().unwrap_or(0)))
            .collect();
        assert!(
            losses.iter().all(|(_, loss)| *loss > 0),
            "{seats}p: the published per-cycle δ must charge EVERY living opponent, which is \
             what makes this the untargeted multiplayer class; measured {losses:?} at beat \
             {beat}"
        );

        // The bound, RECOMPUTED from the offer-beat board.
        let expected_bound = state
            .players
            .iter()
            .filter(|p| !p.is_eliminated)
            .filter_map(|p| {
                let loss = -per_cycle.delta.life.get(&p.id).copied().unwrap_or(0);
                (loss > 0).then(|| (p.life as i64 - 1) / loss)
            })
            .min()
            .expect("at least one seat is losing life, asserted above");
        assert_eq!(
            i64::from(schema.max_iterations),
            expected_bound,
            "{seats}p: CR 704.5a — the published bound must equal `min over living seats of \
             (life - 1) / per-cycle loss`, recomputed here from the offer-beat board"
        );
        assert!(
            schema.is_bounded(),
            "{seats}p: this producer's whole claim is that it NARROWED the bound; \
             max_iterations = {}",
            schema.max_iterations
        );
    }
}

/// PR-7 Phase 5b (G1) — the bounded offer must FORBID a driving period of the PROPOSER'S OWN.
///
/// PAIRED ARMS ON ONE CERTIFYING STATE, differing in exactly one field, asserting opposite
/// outcomes — so no constant implementation passes.
///
/// The retained name states arm ⓑ's contract: the bounded offer must not mint when the recorded
/// period belongs to its proposer. Step (1b) is seat-relative: a non-empty sequence recorded by
/// ANOTHER seat mints the offer, which is what
/// [`a_foreign_driving_period_neither_refuses_nor_recertifies_a_bounded_offer`] directly below
/// asserts.
///
/// WHY THE GUARD IS LOAD-BEARING (measured, not hypothetical): `materialize_fixed_shortcut`
/// EARLY-RETURNS into `materialize_object_growth_shortcut` when the recorded period is the
/// accepting proposal's proposer's own, and the bounded drain path begins strictly below that
/// return. An offer minted while THIS proposer's period is accumulating would be accepted and
/// routed to the object-growth materializer, committing ZERO bounded cycles — the guard converts
/// that silent misroute into an observable refusal. The two conjuncts are NOT disjoint in the
/// tree: the bridge's own gate needs a non-empty STACK, and an on-stack `ActivateAbility` appends
/// to the sequence once a mana activation has armed a period.
///
/// REVERT-PROBE: delete step (1b) ⇒ arm ⓑ returns `Ok(..)` ⇒ FAILS. The refusal is asserted
/// BY REASON (`ProposerHasDrivingPeriod`), not merely as "no offer": an assertion that only
/// observed absence would keep passing if some EARLIER conjunct started refusing first, which
/// is the domination trap.
#[test]
fn a_proposers_own_driving_period_mints_no_bounded_offer() {
    use engine::game::engine::{try_offer_bounded_cycle_shortcut, BoundedOfferRefusal};
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the paired arms need a state that PROVABLY certifies; see the acceptance row");

    // The offer beat's `waiting_for` is the offer itself, so rewind that one field to the
    // Priority beat the offer was raised AT — the bridge's own entry condition.
    let (proposer, _, _) = bounded_offer_parts(&state);
    state.waiting_for = WaitingFor::Priority { player: proposer };

    // ⓐ the state certifies.
    let armed = try_offer_bounded_cycle_shortcut(&state, false);
    assert!(
        armed.is_ok(),
        "REACH-GUARD: arm ⓑ is vacuous unless the SAME state certifies with an empty \
         sequence; got {armed:?}"
    );

    // ⓑ one field reassigned.
    state.last_loop_action_sequence = vec![LoopActionContext {
        card_id: state
            .objects
            .values()
            .next()
            .map(|o| o.card_id)
            .expect("the dump has objects"),
        controller: proposer,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::NotUsed,
        },
        convoke: None,
        pins: vec![],
    }];
    assert_eq!(
        try_offer_bounded_cycle_shortcut(&state, false),
        Err(BoundedOfferRefusal::ProposerHasDrivingPeriod),
        "CR 732.2a: a bounded offer minted with a driving sequence would be routed to the \
         object-growth materializer and commit zero bounded cycles"
    );
}

/// ITEM 2 (CR 732.2a) — a bounded offer is refused by the proposer's OWN driving period, and by
/// NOBODY ELSE'S; and a foreign period is certification-NEUTRAL while it sits there.
///
/// THE BUG THIS ROW PINS. Step (1b) tested `!last_loop_action_sequence.is_empty()`, so a single
/// opponent activation — a period this proposer can neither drive nor benefit from — refused their
/// own certified bounded offer for the rest of the game. CR 732.2a defines a shortcut as "a
/// sequence of game choices, for all players, that may be legally taken based on the current game
/// state and the predictable results of the sequence of choices": another seat's independent
/// activation describes no sequence THIS proposer can take, so it is no reason to refuse theirs.
/// The routing signal was strictly coarser than the admission predicate of the consumer it routes
/// to — `try_offer_object_growth_shortcut` already required every step to belong to the priority
/// holder — so a foreign period could not produce an object-growth offer yet still refused the
/// bounded one. Both now read `GameState::loop_period_controller`.
///
/// FIVE ARMS ON ONE CERTIFYING STATE, differing ONLY in `last_loop_action_sequence`:
///
/// | arm | sequence | expected |
/// |---|---|---|
/// | ⓐ | empty | `Ok` — REACH-GUARD, and the neutrality reference |
/// | ⓑ | proposer's, any card | `Err(ProposerHasDrivingPeriod)` — must-not-flip |
/// | ⓒ | opponent's, any card | `Ok` — **the fix** |
/// | ⓓ | opponent's, Mortality Spear | `Ok` — card-identity independence |
/// | ⓔ | proposer's, Mortality Spear | `Err(ProposerHasDrivingPeriod)` — must-not-flip |
///
/// Refusals are asserted BY REASON, never as bare absence: an assertion that only observed "no
/// offer" would keep passing if some EARLIER conjunct started refusing first (the domination trap
/// `BoundedOfferRefusal` exists for).
///
/// TWO-SIDED CONTROL ON (1b), PER ASSERTION — no constant implementation passes:
/// * **DROP** the proposer comparison (restore `!is_empty()`) ⇒ ⓒ and ⓓ return
///   `Err(ProposerHasDrivingPeriod)` ⇒ THOSE assertions fail, while ⓑ/ⓔ still pass.
/// * **TRIVIALIZE** it constant-refuse (`loop_period_controller().is_some()`) ⇒ ⓒ/ⓓ fail as above.
///   TRIVIALIZE it constant-admit (never refuse) ⇒ ⓑ and ⓔ return `Ok` ⇒ **those** assertions
///   fail instead. Each direction flips a DIFFERENT named assertion.
///
/// CERTIFICATION NEUTRALITY (site E) is folded onto the same arms because it needs the same
/// expensive drive, and reported through the metered seam so the certifying BASIS is observable
/// rather than just `Ok`/`Err`. ⓒ and ⓓ must publish the same `PeriodCertification` and the same
/// `per_cycle.frames_per_period` as ⓐ. ⓒ and ⓓ differ ONLY in the foreign step's `card_id`, so any
/// basis difference between them can arise ONLY from `window_cast_card_ids` feeding gate (5)'s
/// `scope.cast_card_ids` — that pair is itself the discriminator for which mechanism carries the
/// change.
/// * **DROP** the proposer test from `window_cast_card_ids` ⇒ ⓒ certifies `BoardCovered` while
///   ⓐ/ⓓ certify `ResourceSignatureOnly` ⇒ the equality assertion FAILS. That is the harm in one
///   line: an OPPONENT'S choice of which card to activate would select which soundness relief
///   applies to THIS proposer's certification.
/// * **TRIVIALIZE** it to `None` unconditionally ⇒ relief is stripped from the proposer-less 2-arg
///   entry the object-growth detection covers use ⇒ `analysis::resource`'s X4-5 arm (1) fails.
///   (That is why the scoping is `is_some_and`, not `is_some`.)
#[test]
fn a_foreign_driving_period_neither_refuses_nor_recertifies_a_bounded_offer() {
    use engine::game::engine::{
        try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
    };
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the paired arms need a state that PROVABLY certifies; see the acceptance row");

    // The offer beat's `waiting_for` IS the offer, so rewind that one field to the Priority beat
    // the offer was raised at — the mint's own entry condition.
    let (proposer, _, _) = bounded_offer_parts(&state);
    state.waiting_for = WaitingFor::Priority { player: proposer };
    let opp = *engine_live_opponents(&state, proposer)
        .first()
        .expect("REACH-GUARD: the foreign arms need a living opponent to attribute a period to");

    // The X4 subject: a conditioned `ModifyCost`/`SelfRef` static sitting in a zone this window
    // never casts from. Its gate-(5) relief is precisely what an unscoped cast-set read would let
    // an opponent switch on. Looked up BY NAME so the row cannot silently degrade into ⓒ==ⓓ if the
    // fixture's ids ever move.
    let spear = state
        .objects
        .values()
        .find(|o| o.name == "Mortality Spear")
        .map(|o| o.card_id)
        .expect("REACH-GUARD: dump-D ships Mortality Spear; ⓒ-vs-ⓓ is vacuous without it");
    let any_card = state
        .objects
        .values()
        .map(|o| o.card_id)
        .find(|id| *id != spear)
        .expect("REACH-GUARD: ⓒ and ⓓ must differ in card identity");

    let step = |controller: PlayerId, card_id| LoopActionContext {
        card_id,
        controller,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::NotUsed,
        },
        convoke: None,
        pins: vec![],
    };
    // One state, one field reassigned per arm — nothing else differs between arms.
    let mint = |seq: Vec<LoopActionContext>| {
        let mut probe = state.clone();
        probe.last_loop_action_sequence = seq;
        let (outcome, meter) =
            try_offer_bounded_cycle_shortcut_metered(&probe, false, ProbeCap::Shipped);
        let signature = outcome.as_ref().ok().map(|wf| match wf {
            WaitingFor::LoopShortcut { certificate, .. } => {
                certificate
                    .per_cycle
                    .as_ref()
                    .expect(
                        "a bounded offer publishes the per-period signature its bound was \
                             divided by",
                    )
                    .frames_per_period
            }
            other => panic!("expected a bounded LoopShortcut offer, got {other:?}"),
        });
        (outcome, meter.certification, signature)
    };

    // ── ⓐ REACH-GUARD: the SAME state certifies with no sequence at all. Every arm below is
    // vacuous without this, and it is also the reference ⓒ/ⓓ are compared against. ──
    let (empty, empty_basis, empty_k) = mint(vec![]);
    assert!(
        empty.is_ok(),
        "REACH-GUARD: ⓑ–ⓔ are vacuous unless this same state certifies with an empty \
         sequence; got {empty:?}"
    );
    assert!(
        empty_basis.is_some() && empty_k.is_some(),
        "REACH-GUARD: the neutrality assertions compare a BASIS and a period length, so the \
         control must publish both; got {empty_basis:?} / {empty_k:?}"
    );

    // ── ⓑ / ⓔ the proposer's OWN period still refuses, on either card. The load-bearing half of
    // guard (1b) — the silent-misroute prevention — is untouched by the fix. ──
    assert_eq!(
        mint(vec![step(proposer, any_card)]).0,
        Err(BoundedOfferRefusal::ProposerHasDrivingPeriod),
        "ⓑ CR 732.2a: the proposer's OWN accumulating period would route an accepted proposal \
         to the object-growth materializer and commit zero bounded cycles"
    );
    assert_eq!(
        mint(vec![step(proposer, spear)]).0,
        Err(BoundedOfferRefusal::ProposerHasDrivingPeriod),
        "ⓔ the refusal is keyed on WHOSE period it is, not on which card the step names"
    );

    // ── ⓒ / ⓓ THE FIX: a foreign period neither refuses nor moves the certification. ──
    let (c, c_basis, c_k) = mint(vec![step(opp, any_card)]);
    assert!(
        c.is_ok(),
        "ⓒ CR 732.2a: an OPPONENT'S independent activation describes no sequence this proposer \
         can take, so it must not refuse their own certified bounded offer; got {c:?}"
    );
    let (d, d_basis, d_k) = mint(vec![step(opp, spear)]);
    assert!(
        d.is_ok(),
        "ⓓ the admission is keyed on WHOSE period it is, not on which card the foreign step \
         names; got {d:?}"
    );

    assert_eq!(
        (c_basis, c_k),
        (empty_basis, empty_k),
        "ⓒ vs ⓐ — CR 732.2a certification NEUTRALITY: a foreign period must leave the \
         proposer's certification exactly where the empty-sequence control puts it"
    );
    assert_eq!(
        (d_basis, d_k),
        (empty_basis, empty_k),
        "ⓓ vs ⓐ — and neutrality must not depend on WHICH card the opponent activated. ⓒ and ⓓ \
         differ only in `card_id`, so a split here could come only from gate (5)'s \
         `scope.cast_card_ids` — i.e. an opponent selecting this proposer's soundness relief"
    );
}

/// PR-7 Phase 5b — a declared count ABOVE the offered bound is handed back fail-closed.
///
/// **TEST-ONLY ROW, ZERO NEW PRODUCTION CODE.** The guard already ships
/// (`handle_declare_shortcut`'s `Fixed(n) if *n > offer.schema.max_iterations` arm). It was
/// unbuildable before this phase because no producer narrowed the bound below
/// `MAX_SHORTCUT_CYCLES`, so the comparison was inert; the bounded offer is the first
/// producer that can exercise it. Do not read this row as new mechanism.
///
/// REVERT-PROBE: delete that arm ⇒ the over-bound count is accepted, APNAP opens, and the
/// proposal drives past a CR 704.5a threshold INSIDE the proposal ⇒ the zero-elimination
/// assertion FAILS.
/// MUST-NOT-FLIP: `over_cap_fixed_count_hands_back_with_no_drive` (the global-cap arm) and
/// every unbounded offer's acceptance of any `Fixed(n <= MAX)`.
///
/// ⚠ WHAT SEPARATES THE ARMS, and why this row was NON-DISCRIMINATING until fix round 1.
/// `lives_after == lives_before` + zero eliminations is the handback observation — but at
/// `c6d834040` the ACCEPTED, within-bound path produced the identical observation on this same
/// dump, because `materialize_fixed_shortcut` aborted at cycle 0 and committed nothing. The row
/// therefore could not tell "handed back" from "accepted and driven", and would have stayed
/// green with the guard deleted. It discriminates now because the accepted path MOVES LIFE:
/// `bounded_fixed_count_commits_exactly_n_periods` measures `n × δ` committed on this exact
/// dump (`n=1` → `[50,34,30,35]`, `n=3` → `[52,32,28,33]` from `[49,35,31,36]`). That row is
/// this one's positive control; the `WaitingFor::Priority` + no-`RespondToShortcut` assertions
/// below are what separate a handback from a completed drive, since both end at priority.
#[test]
fn declared_count_above_the_offered_bound_is_handed_back() {
    use engine::analysis::decision_template::IterationCount;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the bounded offer must fire; see the acceptance row");
    let (proposer, _, schema) = bounded_offer_parts(&state);
    let bound = schema.max_iterations;
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: this row is about the PER-OFFER bound, so the offer must have narrowed \
         one — at `MAX_SHORTCUT_CYCLES` the global-cap arm would answer instead and the row \
         would test the wrong guard; got {bound}"
    );
    let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
    let eliminated_before = state.players.iter().filter(|p| p.is_eliminated).count();

    let result = apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(bound + 1),
            template: None,
        },
    )
    .expect("the declare is a legal action; it is REFUSED by being handed back, not by Err");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: an over-bound count contains a conditional action, so it is handed back to \
         ordinary priority — no APNAP window, no drive; got {:?}",
        result.waiting_for
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "the CR 732.2b response window must never open for a rejected declaration"
    );
    assert_eq!(
        state.players.iter().filter(|p| p.is_eliminated).count(),
        eliminated_before,
        "ZERO eliminations: the whole reason the bound exists is that a count above it crosses \
         a CR 704.5a threshold inside the proposal"
    );
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        lives_before,
        "zero committed cycles ⇒ no life moved"
    );
}

/// PR-7 Phase 5b — `UntilLethal` against a BOUNDED offer is rejected.
///
/// **TEST-ONLY ROW** for the same reason as the row above: the guard ships already. It is
/// also the D-1 rider — the ONLY test that exercises `handle_declare_shortcut`'s
/// `UntilLethal if offer.schema.is_bounded()` arm, so it is the behavioural proof that
/// swapping the inline `max_iterations < MAX_SHORTCUT_CYCLES` for the shared predicate is
/// semantics-preserving.
///
/// REVERT-PROBES: delete that arm ⇒ an unbounded drive runs past the measured threshold.
/// Invert `ShortcutDecisionSchema::is_bounded()` to `>=` ⇒ THIS row flips too, together with
/// both `phase-ai` rows — one edit to one predicate measurable at every caller. If that
/// inversion leaves this row green, the engine kept a private copy of the comparison.
/// MUST-NOT-FLIP: the whole shipped suite's unbounded offers still accept `UntilLethal`.
///
/// ⚠ SAME NON-DISCRIMINATION CORRECTION as the row above. `lives_after == lives_before` was
/// satisfied by BOTH arms at `c6d834040` (the accepted path committed nothing), so it did not
/// separate "rejected" from "accepted and driven". `bounded_fixed_count_commits_exactly_n_periods`
/// is now the positive control that makes the accepted path observably move life on this dump;
/// the discriminating assertions here are the `Priority` handback plus the absence of a
/// `RespondToShortcut` window, which no accepted declaration produces.
#[test]
fn until_lethal_against_a_bounded_offer_is_rejected() {
    use engine::analysis::decision_template::IterationCount;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the bounded offer must fire; see the acceptance row");
    let (proposer, _, schema) = bounded_offer_parts(&state);
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: the guard under test is keyed on `is_bounded()`, so an unnarrowed offer \
         would take a different arm and the row would be vacuous"
    );
    let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();

    let result = apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        },
    )
    .expect("the declare is a legal action; it is REFUSED by being handed back");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: `UntilLethal` names no count at all, so it cannot be legal against an \
         offer whose producer measured a CR 704 threshold inside the loop; got {:?}",
        result.waiting_for
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "no CR 732.2b window for a rejected declaration"
    );
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        lives_before,
        "zero committed cycles"
    );
}

// ─────────────── PR-7 Phase 5c — the MANDATORY-DRAW cascade, ≥3 players ───────────────

/// Real card (Psychosis Crawler is the printed member of this class); the synthetic name
/// keeps the fixture off the card database.
const BLEEDER: &str = "Whenever you draw a card, each opponent loses 1 life.";
/// Synthetic mandatory payoff. Deliberately NOT "you may draw a card" — the "may" is a
/// genuine CR 603.5 resolution-time choice that step (6) must keep refusing, and this
/// fixture's whole job is to exercise the MANDATORY arm.
const CHRONICLER: &str = "Whenever an opponent loses life, draw a card.";

/// `bloodloop` at N players: a WITHIN-TURN MANDATORY drain cascade whose every cycle also
/// DRAWS, so the board never recurs (a card moves library→hand each cycle) and the offer
/// must come from the growth-cover basis rather than exact recurrence.
///
/// The draw payload is the POINT of the fixture, not an incidental detail. Before this
/// commit, `Effect::Draw` was fail-closed `MayPrompt`, so step (6)
/// `stack_choices_are_all_specified` refused every beat whose stack held one of these
/// triggers and the multiplayer cascade could never be offered. Do NOT "simplify" this to
/// a pure life loop to make it pass — a control that steers around the hole guards nothing
/// about the hole.
fn bloodloop_state(players: u8) -> GameState {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..players {
        scenario.with_life(PlayerId(i), 20);
    }
    scenario.add_creature_from_oracle(P0, "Test Bleeder", 2, 2, BLEEDER);
    scenario.add_creature_from_oracle(P0, "Test Chronicler", 2, 2, CHRONICLER);
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for i in 0..players {
        scenario.with_library_top(PlayerId(i), &refs);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state().clone()
}

/// PR-7 Phase 5c ACCEPTANCE — the bounded CR 732.2a offer fires on a MANDATORY-DRAW
/// cascade at 2, 3 and 4 players.
///
/// # What flips when the widening is reverted
///
/// Measured at HEAD `ea6000b5c` with the same fixture and the same driver: 3p and 4p mint
/// **`ENGINE_OFFERS = 0`**, every candidate beat refused `UnspecifiedChoiceWindow` (34
/// bridge-moment refusals on each), while certification itself succeeded. Classify
/// `Effect::Draw` back to `MayPrompt` and `drive_to_bounded_offer` returns `None` for
/// `players >= 3` — `expect` panics and this test is RED. The 2p row discriminates too, on
/// the beat: the offer moves 29 → 31 under the revert, because step (6) starts passing one
/// full drain cycle earlier once the draw entries stop refusing.
///
/// # Why the beats are pinned literals
///
/// The drive is deterministic — fixed scenario seed, `dump_drive_one_beat`'s policy is
/// total (always pass at `Priority`, first legal answer otherwise), and no RNG is on the
/// path. Re-run identical across runs. An offer beat is observable behaviour that this
/// commit CHANGED; pinning it is what stops a later change from moving it silently.
#[test]
fn bloodloop_mandatory_draw_cascade_offers_at_2p_3p_and_4p() {
    for (players, expected_beat, expected_turn) in [(2u8, 37usize, 4u32), (3, 76, 5), (4, 129, 6)] {
        let mut state = bloodloop_state(players);
        let beat = drive_to_bounded_offer(&mut state, 400).unwrap_or_else(|| {
            panic!(
                "{players}p mandatory-draw cascade must raise a bounded offer; a `None` here is \
                 the pre-widening behaviour (step (6) refusing every draw-bearing stack)"
            )
        });
        assert_eq!(beat, expected_beat, "{players}p offer beat");
        assert_eq!(state.turn_number, expected_turn, "{players}p offer turn");

        let (proposer, certificate, schema) = bounded_offer_parts(&state);
        assert_eq!(
            proposer, P0,
            "{players}p: the cascade's controller proposes"
        );
        assert!(
            schema.is_bounded(),
            "{players}p: an unbounded schema would take a different declare arm"
        );
        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .expect("a bounded offer states its per-period signature");
        assert_eq!(
            per_cycle.frames_per_period, 2,
            "{players}p: this cascade's derived period spans two retained ring frames. A WIDTH \
             tripwire only — since fix round 1 both bases measure the span, so no value \
             attributes a basis (see `bounded_offer_on_a_within_turn_draw_drain_is_basis_b`)"
        );

        // The multiplayer property the ≥3p rows exist for: ONE cycle charges EVERY
        // opponent, not just the first. A 2p-only guard could not see this.
        let opponents: Vec<PlayerId> = state
            .players
            .iter()
            .map(|p| p.id)
            .filter(|p| *p != P0)
            .collect();
        assert_eq!(opponents.len(), usize::from(players) - 1);
        for opponent in &opponents {
            assert_eq!(
                per_cycle.delta.life.get(opponent).copied(),
                Some(-1),
                "{players}p: one cycle drains {opponent:?}"
            );
        }
        assert_eq!(
            per_cycle.delta.life.get(&P0).copied().unwrap_or(0),
            0,
            "{players}p: the cascade's controller loses no life"
        );
    }
}

// ═══════════════ FIX ROUND 1 — the declared count is CONSUMABLE ═══════════════
//
// Before this round `materialize_fixed_shortcut`'s `'cycles: for i in 0..n` advanced only on
// `CycleOutcome::Recurred`, which needs board RECURRENCE. A basis-B certificate — what
// `ring_delta_signature` mints, and the whole class `try_offer_bounded_cycle_shortcut` widened
// offers to — certifies a periodic DELTA, not a recurring board, so neither recurrence
// predicate can ever fire and `n` was structurally inert. MEASURED at `c6d834040` with the same
// fixtures and the same production entry (`apply` → declare → APNAP accepts):
//
//   bloodloop3 n=1 → [20,0,0] GameOver{P0} elim=2 │ n=3 → [20,0,0] GameOver{P0} elim=2
//   bloodloop4 n=1 → [20,0,0,0] elim=3           │ n=3 → [20,0,0,0] elim=3
//   dina    4p n=1 → [49,35,31,36] (unchanged)   │ n=3 → [49,35,31,36] (unchanged)
//
// `Fixed(1)` and `Fixed(3)` byte-identical — either the table dies or nothing commits. The
// rows below are the antidote: the same trajectories, with `n` bound to the OBSERVED board
// delta and to the OFFER's own published signature, never to a literal.

/// Declare `Fixed(n)` on the bounded offer `state` is parked at, accept with every living
/// opponent through `apply()`, and return the per-seat life delta the drive COMMITTED
/// alongside the signature and bound the offer published.
///
/// Everything is read off the production state: the signature comes from the offer the ENGINE
/// wrote, the delta from `Player::life` before and after. Nothing is recomputed by the test.
fn accept_bounded_fixed(
    state: &mut GameState,
    n: u32,
) -> (
    Vec<(PlayerId, i64)>,
    engine::analysis::resource::PeriodicDelta,
    u32,
) {
    let (proposer, certificate, schema) = bounded_offer_parts(state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes the per-period signature its bound was divided by");
    let bound = schema.max_iterations;
    let before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();
    r6a_declare_and_accept_all(state, proposer, n);
    let committed: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .zip(&before)
        .map(|(p, (id, l0))| {
            assert_eq!(
                p.id, *id,
                "the seat vector is positional and never reordered"
            );
            (p.id, p.life as i64 - l0)
        })
        .collect();
    (committed, per_cycle, bound)
}

/// FIX ROUND 1 PRIMARY (HIGH-1/HIGH-2/HIGH-3) — an accepted `Fixed(n)` within the offered
/// bound commits **exactly `n` copies of the published per-period delta**, on the real 4p
/// Dina/Conqueror dump and on the synthetic mandatory-draw cascade at 3 and 4 players.
///
/// # What the assertion is bound to
///
/// `n * per_cycle.delta.life[seat]`, derived from the certificate the ENGINE published at the
/// offer beat. No literal life total appears in an equality. A fixture whose drain rate drifts
/// moves both sides together; a drive that commits the wrong NUMBER of periods moves only one.
///
/// # Why this is not vacuous
///
/// * Reach-guards below establish that the published δ is non-zero, that at least two seats
///   carry a non-zero term (so a 2-player-shaped bug cannot hide), and that the bound leaves
///   room for `n = 3` — without which the `n`-scaling is untestable.
/// * The `n = 1` vs `n = 3` boards are asserted DIFFERENT on the same fixture. That single
///   assertion is the direct antidote to the defect: at `c6d834040` they were byte-identical.
///
/// # REVERT-PROBES (each RUN, each FLIPPED)
///
/// * ⓐ delete `|| frames_per_period.is_some_and(|k| frames_this_cycle >= k)` from
///   `drive_one_shortcut_cycle` ⇒ HEAD behaviour returns: dina commits ZERO (`Abort` at cycle
///   0, delta `[0,0,0,0]`) and bloodloop cross-lethals the whole table. Every `assert_eq!` on
///   the committed delta FAILS, and so does `no seat is eliminated`.
/// * ⓑ replace the delimiter's `k` with a hardcoded `1` ⇒ bloodloop (whose derived period is
///   `k == 2` ring frames) commits HALF-periods: `n` cycles deliver `n/2` copies of δ. The
///   3p/4p `assert_eq!` FAILS while the `k == 1` dina row stays green — which is what proves
///   the row reads the VALUE of `frames_per_period` and not merely its presence.
#[test]
fn bounded_fixed_count_commits_exactly_n_periods() {
    /// Rebuilt from scratch per `n` — a driven trajectory is not replayable from a used state.
    fn build(name: &str) -> GameState {
        match name {
            "dina_conqueror_4p" => restore_dump(&gunzip_dump(include_bytes!(
                "../fixtures/dina_conqueror_4p.json.gz"
            ))),
            "bloodloop3" => bloodloop_state(3),
            "bloodloop4" => bloodloop_state(4),
            other => panic!("unknown fixture {other}"),
        }
    }

    for name in ["dina_conqueror_4p", "bloodloop3", "bloodloop4"] {
        let mut boards: Vec<Vec<(PlayerId, i64)>> = vec![];
        for n in [1u32, 3] {
            let mut state = build(name);
            drive_to_bounded_offer(&mut state, 400).unwrap_or_else(|| {
                panic!("{name}: the bounded offer must fire; see the acceptance row")
            });

            let (committed, per_cycle, bound) = accept_bounded_fixed(&mut state, n);

            // ── reach-guards: without these the equality below can pass degenerately ──
            assert!(
                per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
                "{name}: a zero-delta period makes `n * δ` zero for every `n`, so the scaling \
                 assertion would hold for a drive that committed nothing"
            );
            assert!(
                per_cycle.delta.life.values().filter(|v| **v != 0).count() >= 2,
                "{name}: fewer than two seats with a non-zero life term is a 2-player shape; \
                 the whole class exists because a MULTIPLAYER drain crowns nobody. got {:?}",
                per_cycle.delta.life
            );
            assert!(
                bound >= 3,
                "{name}: `n = 3` must be WITHIN the offered bound, else the declaration is \
                 handed back and this row silently tests the rejection arm; bound = {bound}"
            );
            assert!(
                per_cycle.frames_per_period >= 1,
                "{name}: a period spans at least one retained ring frame; got {}",
                per_cycle.frames_per_period
            );

            // ── THE PROPERTY: committed delta == n × published per-period delta ──
            for (seat, delta) in &committed {
                assert_eq!(
                    *delta,
                    i64::from(n) * per_cycle.delta.life.get(seat).copied().unwrap_or(0),
                    "{name} n={n}: {seat:?}'s committed life delta must be exactly `n` copies \
                     of the period the offer published ({:?}); committed {committed:?}",
                    per_cycle.delta.life
                );
            }

            // ── the bound's own contract: CR 704.5a headroom is `life - 1`, so no seat may
            //    be eliminated by a within-bound count ──
            assert_eq!(
                state.players.iter().filter(|p| p.is_eliminated).count(),
                0,
                "{name} n={n}: CR 704.5a — `min over living seats of (life - 1) / loss` \
                 reserves one point of headroom, so a within-bound drive eliminates nobody"
            );
            assert!(
                state.players.iter().all(|p| p.life > 0),
                "{name} n={n}: every seat is above the CR 704.5a threshold; lives {:?}",
                state.players.iter().map(|p| p.life).collect::<Vec<_>>()
            );
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "{name} n={n}: a completed finite drive hands back to ordinary priority \
                 (CR 800.4a living seat), not a terminal state; got {:?}",
                state.waiting_for
            );

            boards.push(committed);
        }

        // ── THE DISCRIMINATOR. At `c6d834040` these two were identical for every fixture.
        assert_ne!(
            boards[0], boards[1],
            "{name}: `Fixed(1)` and `Fixed(3)` must produce MEASURABLY different boards — \
             identical outcomes are exactly the defect this round fixes"
        );
    }
}

/// ITEM 2 (CR 732.2a) — the ACCEPT side: a foreign driving period in state must not divert an
/// accepted bounded grant into the object-growth materializer.
///
/// **WHY NO ROW HAS EVER STARTED FROM A BOARD CARRYING ONE.**
/// `GameState::migrate_transient_loop_sequence` clears `last_loop_action_sequence` at every load
/// whose `waiting_for` is not a shortcut window, so every dump-driven row in this file begins
/// from a cleared field. The whole accept-side dispatch on that field is therefore untested — the
/// blindness is in the FIXTURE PIPELINE, not in the rows. The answer is injection into a tracked
/// fixture (as `a_proposers_own_driving_period_mints_no_bounded_offer` already does), not a new
/// tracked dump.
///
/// **WHY THIS IS THE ACCEPT SEAM AND NOT THE MINT SEAM.** The mint arms establish that a foreign
/// period no longer REFUSES the offer. That relaxation is only safe if the thing subsequently
/// accepted still routes to the DRAIN materializer: `materialize_fixed_shortcut` early-returns
/// into `materialize_object_growth_shortcut` on its routing test, and the bounded drain path
/// begins strictly below that return. A mint-seam row cannot see which side of it the accept
/// lands on.
///
/// **SITE F IS NOT ON THIS PATH, and that is asserted rather than assumed** — dina's bounded offer
/// publishes an EMPTY point set, so `handle_declare_shortcut`'s
/// `if !offer.schema.points.is_empty()` block is skipped whole and the `template: None`
/// declaration this row makes never reaches the declare-seam arm. Site F's own row lives on the
/// F4 fixture for exactly the complementary reason.
///
/// **THE PROPERTY**: the committed life delta is exactly `n ×` the published per-period delta —
/// i.e. the drain materializer ran. Positive control on the same fixture and same helper:
/// [`bounded_fixed_count_commits_exactly_n_periods`], whose reach-guards (non-zero δ, ≥ 2 seats
/// moving, bound ≥ 3) are repeated here because without them `n × δ` is satisfied by a drive that
/// committed nothing.
///
/// **TWO-SIDED CONTROL:**
/// * **DROP** the proposer test at the materialize dispatch (restore `!is_empty()`) ⇒ the accept
///   early-returns into `materialize_object_growth_shortcut` and commits ZERO ⇒ `n × δ` fails for
///   every seat with a non-zero rate.
/// * **TRIVIALIZE** it to always take the drain path ⇒ a genuine object-growth accept commits
///   nothing, which the object-growth siblings of the positive control catch.
#[test]
fn an_accepted_bounded_grant_drains_even_with_a_foreign_period_in_state() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    const N: u32 = 3;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the bounded offer must fire; see the acceptance row");

    let (proposer, _, schema) = bounded_offer_parts(&state);
    assert!(
        schema.points.is_empty(),
        "REACH-GUARD / SCOPE: this row isolates the MATERIALIZE dispatch. A non-empty point set \
         would drag `handle_declare_shortcut`'s declare-seam arm into the same measurement and \
         the outcome would no longer attribute to one site; got {:?}",
        schema.points
    );
    let opp = *engine_live_opponents(&state, proposer)
        .first()
        .expect("REACH-GUARD: the foreign period needs a living opponent to belong to");

    // THE INJECTION the load migration hides from every dump-driven row: an opponent's own
    // recorded period, sitting in state at the moment the grant is accepted.
    state.last_loop_action_sequence = vec![LoopActionContext {
        card_id: state
            .objects
            .values()
            .next()
            .map(|o| o.card_id)
            .expect("the dump has objects"),
        controller: opp,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::NotUsed,
        },
        convoke: None,
        pins: vec![],
    }];
    assert_ne!(
        opp, proposer,
        "REACH-GUARD: a period injected for the PROPOSER would be the legitimate object-growth \
         route, and this row would assert the opposite of what it means to"
    );

    let (committed, per_cycle, bound) = accept_bounded_fixed(&mut state, N);

    // ── reach-guards: without these `n × δ` holds degenerately for a drive that did nothing ──
    assert!(
        per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
        "a zero-delta period makes `n × δ` zero for every `n`, so the equality below would hold \
         for a drive that committed nothing — which is the exact failure mode this row exists to \
         catch"
    );
    assert!(
        per_cycle.delta.life.values().filter(|v| **v != 0).count() >= 2,
        "fewer than two seats with a non-zero life term is a 2-player shape; got {:?}",
        per_cycle.delta.life
    );
    assert!(
        bound >= N,
        "`n = {N}` must be WITHIN the offered bound, else the declaration is handed back and \
         this row silently tests the rejection arm; bound = {bound}"
    );

    // ── THE PROPERTY: the DRAIN materializer ran, not the object-growth one ──
    for (seat, delta) in &committed {
        assert_eq!(
            *delta,
            i64::from(N) * per_cycle.delta.life.get(seat).copied().unwrap_or(0),
            "CR 732.2a: with a FOREIGN period in state the accepted grant must still commit \
             exactly `n` copies of the published per-period delta ({:?}). A zero here is the \
             object-growth misroute: `materialize_fixed_shortcut` early-returned into \
             `materialize_object_growth_shortcut`, which commits no bounded cycles at all. \
             {seat:?} committed {committed:?}",
            per_cycle.delta.life
        );
    }
}

/// ITEM 2 ROUND 2 (CR 732.2a) — the DECLINE seam: one seat's decline may discard only its OWN
/// recorded period, never another seat's.
///
/// **A SHAPE THE PRE-FIX TREE COULD NOT EXPRESS, which is why no existing row can supply it.**
/// While step (1b) refused on mere non-emptiness, no `WaitingFor::LoopShortcut` could coexist with
/// a period belonging to anyone but its proposer — the object-growth producer mints only for the
/// period's own controller, and the bounded producer minted only with the field empty. So
/// `handle_decline_shortcut`'s unconditional `last_loop_action_sequence.clear()` was, by
/// construction, only ever able to clear the decliner's own. The seat-relative (1b) makes the
/// two-seat state reachable, and `DeclineShortcut` dispatches from ANY `LoopShortcut` — it is the
/// AI's only action at a bounded offer — so an unconditional clear became one seat's decline
/// wiping another seat's accumulating period, suppressing THAT seat's offer until it re-armed.
///
/// **THE TWO ARMS, on one real driven bounded offer, differing ONLY in the injected period's
/// controller** — so no constant implementation passes:
///
/// | arm | injected period | assertion |
/// |---|---|---|
/// | FOREIGN | an opponent's | SURVIVES the decline (**the fix**) |
/// | OWN | the proposer's | CLEARED by the decline (must-not-flip: the load-bearing Seam-2 suppressor) |
///
/// **TWO-SIDED CONTROL, PER ASSERTION — each direction flips a DIFFERENT named assertion:**
/// * **DROP** the ownership test (restore the unconditional
///   `state.last_loop_action_sequence.clear()`) ⇒ the FOREIGN arm's survival assertion FAILS,
///   while OWN still passes.
/// * **TRIVIALIZE** it to never clear (delete the clear, or gate it on
///   `loop_period_controller().is_none()`) ⇒ the OWN arm's clear assertion FAILS, while FOREIGN
///   still passes.
///
/// The decline is driven through the production `apply()` reducer, not by calling the handler, so
/// the post-return reconcile runs too: the OWN arm therefore also proves the clear still suppresses
/// re-offer within the same `apply()` (a re-nag would leave `waiting_for` on a `LoopShortcut`), and
/// the FOREIGN arm proves leaving a foreign period in place does not resurrect one.
#[test]
fn declining_a_shortcut_discards_only_the_decliners_own_driving_period() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    // One real driven bounded offer, re-derived per arm so neither arm inherits the other's board.
    let offer_state = || {
        let mut state = restore_dump(&gunzip_dump(include_bytes!(
            "../fixtures/dina_conqueror_4p.json.gz"
        )));
        drive_to_bounded_offer(&mut state, 400)
            .expect("the bounded offer must fire; see the acceptance row");
        state
    };
    let period_of = |controller: PlayerId, state: &GameState| {
        vec![LoopActionContext {
            card_id: state
                .objects
                .values()
                .next()
                .map(|o| o.card_id)
                .expect("the dump has objects"),
            controller,
            action: LoopAction::Recast {
                from_zone: engine::types::zones::Zone::Hand,
                uses_buyback: BuybackUsage::NotUsed,
            },
            convoke: None,
            pins: vec![],
        }]
    };

    // ── FOREIGN: seat B's period is mid-accumulation when seat A declines ──
    let mut state = offer_state();
    let (proposer, _, _) = bounded_offer_parts(&state);
    let opp = *engine_live_opponents(&state, proposer)
        .first()
        .expect("REACH-GUARD: the foreign period needs a living opponent to belong to");
    assert_ne!(
        opp, proposer,
        "REACH-GUARD: a period injected for the PROPOSER would be the OWN arm, and this arm \
         would assert the opposite of what it means to"
    );
    state.last_loop_action_sequence = period_of(opp, &state);
    assert_eq!(
        state.last_loop_action_sequence.len(),
        1,
        "REACH-GUARD: the arm is vacuous unless a period is actually accumulating when the \
         decline lands — nothing survives an empty field"
    );

    apply(&mut state, proposer, GameAction::DeclineShortcut)
        .expect("the proposer may always decline their own offer (CR 732.2a)");
    assert_eq!(
        state
            .last_loop_action_sequence
            .iter()
            .map(|s| s.controller)
            .collect::<Vec<_>>(),
        vec![opp],
        "CR 732.2a: {proposer:?} declining their own offer must leave {opp:?}'s accumulating \
         period intact — a recorded period is evidence about the seat that recorded it, and \
         discarding it here suppresses THAT seat's own offer until it re-arms"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: preserving {opp:?}'s foreign period must not re-offer {proposer:?}'s declined \
         shortcut within the same `apply()`; got {:?}",
        state.waiting_for
    );

    // ── OWN: the must-not-flip half. The Seam-2 suppressor is load-bearing for the decliner. ──
    let mut state = offer_state();
    let (proposer, _, _) = bounded_offer_parts(&state);
    state.last_loop_action_sequence = period_of(proposer, &state);
    assert_eq!(
        state.last_loop_action_sequence.len(),
        1,
        "REACH-GUARD: the arm is vacuous unless a period is actually accumulating when the \
         decline lands — an already-empty field is cleared by doing nothing"
    );

    apply(&mut state, proposer, GameAction::DeclineShortcut)
        .expect("the proposer may always decline their own offer (CR 732.2a)");
    assert!(
        state.last_loop_action_sequence.is_empty(),
        "CR 732.2a: the decliner's OWN period must still be discarded — without it the \
         post-return reconcile re-fires `try_offer_object_growth_shortcut` inside this same \
         `apply()` and re-nags the offer just declined. seq = {:?}",
        state.last_loop_action_sequence
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "and the declined offer must not have been re-raised within the same `apply()`; got {:?}",
        state.waiting_for
    );
}

/// CR 732.2a — the SECOND unconditional cross-seat clear in the teardown family:
/// `until_lethal_fallback`. An aborted `UntilLethal` drive discards only the PROPOSER'S own period.
///
/// **WHY THIS ROW EXISTS NOW.** Round 2 found this site and refused it on a reachability argument
/// that ended in "no fixture reaches it". That was a fact about the fixture corpus, not about the
/// engine: `until_lethal_fallback` starts with `*state = committed`, restoring the PRE-DRIVE board
/// — and since step (1b) went seat-relative, that board can carry another seat's period. MEASURED
/// on the shipped tree before the guard landed: with a foreign period injected at the accept, the
/// sprout-swarm `UntilLethal` drive aborts, the fallback runs, and the foreign seat's period comes
/// back length 0. Same defect, same seam family, same one-line authority as
/// [`declining_a_shortcut_discards_only_the_decliners_own_driving_period`] above.
///
/// **WHY THE OBJECT-GROWTH FIXTURE.** The fallback is reached only when the drive refuses to crown.
/// `object_growth_advantage_untillethal_no_crown` is the tree's own proof that this board does
/// exactly that (an inert Advantage token loop has no faller), so both arms below are the shipped
/// abort path with one field changed — not a synthesized failure.
///
/// | arm | injected period | assertion |
/// |---|---|---|
/// | FOREIGN | an opponent's | SURVIVES the aborted drive (**the fix**) |
/// | OWN | the proposer's | CLEARED by it (must-not-flip: the anti-livelock suppressor the doc names) |
///
/// **TWO-SIDED CONTROL, PER ASSERTION — each direction flips a DIFFERENT named assertion:**
/// * **DROP** the ownership test (restore the unconditional `last_loop_action_sequence.clear()`)
///   ⇒ the FOREIGN arm's survival assertion FAILS, while OWN still passes.
/// * **TRIVIALIZE** it to never clear ⇒ the OWN arm's clear assertion FAILS, while FOREIGN passes.
#[test]
fn an_aborted_until_lethal_drive_discards_only_the_proposers_own_driving_period() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    // The shipped abort path, re-derived per arm: cast the recast, take the object-growth offer,
    // declare `UntilLethal` (the AI's hardcoded shape), and let every opponent accept.
    let offer_state = || {
        let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
        let _ = runner
            .cast(sprout)
            .accept_optional()
            .convoke_with(&[fodder[0]])
            .commit()
            .resolve();
        let WaitingFor::LoopShortcut { proposer, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "REACH-GUARD: the object-growth cast must OFFER, got {:?}",
                runner.state().waiting_for
            )
        };
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::UntilLethal,
                template: None,
            })
            .expect("the proposer declares UntilLethal on its own object-growth offer");
        (runner, proposer)
    };
    let period_of = |controller: PlayerId, runner: &GameRunner| {
        vec![LoopActionContext {
            card_id: runner
                .state()
                .objects
                .values()
                .next()
                .map(|o| o.card_id)
                .expect("the scenario has objects"),
            controller,
            action: LoopAction::Recast {
                from_zone: engine::types::zones::Zone::Hand,
                uses_buyback: BuybackUsage::NotUsed,
            },
            convoke: None,
            pins: vec![],
        }]
    };

    // ── FOREIGN: seat B's period is mid-accumulation when seat A's drive aborts ──
    let (mut runner, proposer) = offer_state();
    let opp = runner
        .state()
        .players
        .iter()
        .map(|p| p.id)
        .find(|p| *p != proposer)
        .expect("REACH-GUARD: the foreign period needs a second seat to belong to");
    runner.state_mut().last_loop_action_sequence = period_of(opp, &runner);
    accept_all_opponents(&mut runner);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "REACH-GUARD: this row is about the FALLBACK, so the drive must refuse to crown — a \
         crowned drive never reaches the clear and the assertion below would be vacuous; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .last_loop_action_sequence
            .iter()
            .map(|s| s.controller)
            .collect::<Vec<_>>(),
        vec![opp],
        "CR 732.2a: {proposer:?}'s aborted drive must leave {opp:?}'s accumulating period intact. \
         `until_lethal_fallback` rolls the board back to the pre-drive `committed` state, which \
         carries that period, and an unconditional clear then destroys it as a side effect of \
         somebody else's abort"
    );

    // ── OWN: the must-not-flip half. The clear is the anti-livelock suppressor for the proposer. ──
    let (mut runner, proposer) = offer_state();
    assert_eq!(
        runner
            .state()
            .last_loop_action_sequence
            .iter()
            .map(|s| s.controller)
            .collect::<Vec<_>>(),
        vec![proposer],
        "REACH-GUARD: the real recast must have armed the PROPOSER'S own period, else this arm \
         tests an empty field that is cleared by doing nothing"
    );
    accept_all_opponents(&mut runner);
    assert!(
        runner.state().last_loop_action_sequence.is_empty(),
        "CR 732.2a: the proposer's OWN period must still be discarded — without it the reconcile \
         re-fires `try_offer_object_growth_shortcut` on the loop just abandoned and livelocks. \
         seq = {:?}",
        runner.state().last_loop_action_sequence
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "and the abandoned offer must not have been re-raised; got {:?}",
        runner.state().waiting_for
    );
}

/// FIX ROUND 2 (MED-2) — the same `n × δ` property on a certification-basis **A** offer, at
/// DRIVE level. The row above covers basis **B** on all three of its fixtures; every basis-A
/// claim in this lane rested on ONE published-number assertion until this row.
///
/// # Why the basis matters, and what was uncovered
///
/// `frames_per_period` reaches the drive from two different producers.
/// Basis B derives it from `ring_delta_signature` (the ring window's own period `k`); basis A
/// derives it from the certifying prior's ring index. Fix round 1 changed ONLY the basis-A
/// producer — a hardcoded `1` became the measured span — and MEASURED, reverting that hardcode
/// flips exactly one of the **83** rows that existed in this file's `loop_shortcut::` module
/// BEFORE this row: [`interactive_3p_subset_lethal_does_not_crown`]. ⚠ On THIS tree the count is
/// **2 of 85** — the second being this row, by design (its probe ⓐ below IS that revert). Fix
/// round 3 (LOW-1) corrected the earlier wording "one row of the 85", which took its numerator
/// from the pre-commit tree and its denominator from the post-commit one — two epochs in one
/// sentence. Runner and filter for the flip claim:
/// `cargo test -p phase-engine --test integration -- loop_shortcut::` (module filter on the
/// `integration` binary) — and that runner is also the AUTHORITY for the denominator: it
/// reports **85 passed / 0 failed / 4090 filtered out** on this tree, re-run in fix round 4 at
/// `025015135`. The 83 is the same module at `bc20d4ff4`.
///
/// ⚠ The denominator is anchored to the RUNNER and not, as fix round 3 wrote it, to a grep of
/// the test-attribute literal over this file (fix round 4, LOW-4). That grep is contaminable by
/// this very doc: round 3's own correction quoted the literal inside this comment, so at
/// `025015135` the grep returned **86** while the runner still reported 85, and a reader
/// applying the stated method would have concluded the doc was stale. This round dropped the
/// quoted literal, so the two agree again — but the runner counts ROWS and the grep counts
/// MENTIONS, and only one of those is what "of 85" means. That row asserts the PUBLISHED VALUE and
/// nothing else; it never declares a count, so nothing in the tree observed what a basis-A
/// offer's drive actually commits. The claim "under the hardcode that fixture's accepted drive
/// committed nothing at all" was true and untracked. This row tracks it.
///
/// # The fixture, and why it is the right one
///
/// `setup_3p_subset_lethal` is the ONE basis-A fixture whose published span is not 1: the
/// `DRAIN_CLERIC` / `BLOOD_SIPPER` pairing alternates a gain-life resolution and a lose-life
/// resolution, so one whole repetition spans TWO retained ring frames (`frames_per_period == 2`,
/// asserted below as a reach-guard). A fixture with `k == 1` could not tell a drive that reads
/// the VALUE from one that reads any positive constant.
///
/// ⚠ SCOPE (fix round 3, LOW-4): this coverage is **synthetic-only**. All three basis-A rows in
/// this file are `GameScenario` builds; **no real dump certifies on basis A** — the file's real
/// 4p dumps are basis B, `dina_untargeted_drain_4p_offers_at_three_live_opponents` measured so
/// two ways in its own doc. This repo's standing lesson is real-dump-over-synthetic, so the row
/// says which it is rather than letting a reader take it for real-game evidence. Building a real
/// basis-A dump is its own round, not this one.
///
/// # MEASURED, through the production accept path (`apply` → declare → APNAP accepts)
///
/// derived `k = 2` ⇒ `n=1` commits `{P0:+1, P1:-1, P2:0}`, `n=2` `{+2,-2,0}`, `n=3` `{+3,-3,0}`
/// — exactly `n × δ`. P2 is the life-loss-immune bystander and is untouched at every `n`, which
/// is the multiplayer half: one cycle charges the seats the certificate names and only those.
/// The governing rule is **CR 101.2** — `LIFE_LOSS_IMMUNE` is "Your life total can't change.",
/// a "can't" effect, which takes precedence over the trigger's life-loss instruction. (Fix
/// round 3, LOW-2: this line cited CR 119.8, which governs life EXCHANGES, life REDISTRIBUTION,
/// and pay-life COSTS — none of which happens here. `setup_3p_bystander_winner` above already
/// names 101.2 as governing, with 119.8 only as a `cf.`, and 101.2 is the engine's own
/// convention for "can't" overrides.)
///
/// # REVERT-PROBES — both RUN, and the second one does NOT flip
///
/// * ⓐ **FLIPS.** Restore basis A's hardcoded `frames_per_period: 1` ⇒ every `n` commits
///   `{P0: 0, P1: 0, P2: 0}`, and the row reports it at the non-zero-commit guard. The
///   mechanism: `frames_per_period` is an OR-ed delimiter, so a `k` SMALLER than the true span
///   cuts the cycle early — here at one frame, half a period — and
///   `materialize_fixed_shortcut`'s conformance check then drops every one of them.
/// * ⓑ **DOES NOT FLIP**, measured, and it is recorded rather than quietly dropped. Deleting
///   `|| frames_per_period.is_some_and(|k| frames_this_cycle >= k)` from
///   `drive_one_shortcut_cycle` leaves this row GREEN, because this is a basis-**A** fixture:
///   its board genuinely RECURS, so `loop_states_equal_modulo_resources(boundary, &norm)` is a
///   working delimiter on its own and lands on the same two-frame cycle. (The same probe flips
///   five other rows in this module, including
///   `bounded_fixed_drive_rolls_back_a_partial_crossing_cycle` — the basis-B fixtures, whose
///   boards never recur, are the ones that need the delimiter to exist at all.)
///
/// So this row's discrimination rests ENTIRELY on ⓐ — which is the point: ⓐ is the only edit
/// that distinguishes a measured span from a hardcoded one, and before this row nothing in the
/// tree observed its drive-level consequence.
#[test]
fn basis_a_bounded_fixed_count_commits_exactly_n_periods() {
    let mut boards: Vec<Vec<(PlayerId, i64)>> = vec![];
    for n in [1u32, 2, 3] {
        // Rebuilt per `n`: a driven trajectory is not replayable from a used state.
        let (mut runner, kickoff) = setup_3p_subset_lethal(LoopDetectionMode::Interactive);
        let _ = runner.cast(kickoff).resolve();
        drive_scenario_to_bounded_offer(&mut runner, PRIMED_LOOP_BEATS).unwrap_or_else(|| {
            panic!(
                "the subset-lethal class raises a bounded offer (see \
                 `interactive_3p_subset_lethal_does_not_crown`); got {:?}",
                runner.state().waiting_for
            )
        });
        let mut state = runner.state().clone();

        let (committed, per_cycle, bound) = accept_bounded_fixed(&mut state, n);

        // ── reach-guards: without these the `n × δ` equality can pass degenerately ──
        assert!(
            per_cycle.delta.life.values().filter(|v| **v != 0).count() >= 2,
            "a zero-or-single-term δ makes `n × δ` trivially satisfiable; got {:?}",
            per_cycle.delta.life
        );
        assert!(
            bound >= 3,
            "`n = 3` must be WITHIN the offered bound, else the declaration is handed back and \
             this row silently tests the rejection arm; bound = {bound}"
        );
        // THE ANTIDOTE TO THE HARDCODE: the drive must commit something. Under
        // `frames_per_period: 1` on this fixture the conformance check drops every half-period
        // and this is `{0,0,0}` — a state in which the `n × δ` equality below still holds for
        // the zero-δ seats and would not, on its own, notice.
        assert!(
            committed.iter().any(|(_, delta)| *delta != 0),
            "n={n}: a basis-A drive that commits nothing is the hardcoded-span defect; \
             committed {committed:?}"
        );

        // ── THE PROPERTY: committed delta == n × published per-period delta ──
        for (seat, delta) in &committed {
            assert_eq!(
                *delta,
                i64::from(n) * per_cycle.delta.life.get(seat).copied().unwrap_or(0),
                "n={n}: {seat:?}'s committed life delta must be exactly `n` copies of the \
                 period the offer published ({:?}); committed {committed:?}",
                per_cycle.delta.life
            );
        }

        assert_eq!(
            state.players.iter().filter(|p| p.is_eliminated).count(),
            0,
            "n={n}: CR 704.5a — a within-bound drive eliminates nobody"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "n={n}: a completed finite drive hands back to ordinary priority; got {:?}",
            state.waiting_for
        );

        // ── WIDTH TRIPWIRE, deliberately LAST. It is the same published-number observation
        //    `interactive_3p_subset_lethal_does_not_crown` already carries, and MED-2's whole
        //    finding is that a published number is not a drive-level fact. Placed after the
        //    assertions above so that under the hardcoded-span revert the failure this row
        //    REPORTS is "the drive committed nothing", not "the offer published 1" — measured:
        //    with it placed first, the hardcode probe failed here and the drive assertions were
        //    never reached, which would have made this row a second copy of the existing one.
        assert_eq!(
            per_cycle.frames_per_period, 2,
            "n={n}: this fixture's repetition spans two retained ring frames; a drift changes \
             what one committed cycle means and every equality above with it"
        );
        boards.push(committed);
    }

    // Three DISTINCT boards. Under the hardcoded span all three are `{0,0,0}`.
    assert_ne!(boards[0], boards[1], "`Fixed(1)` vs `Fixed(2)`");
    assert_ne!(boards[1], boards[2], "`Fixed(2)` vs `Fixed(3)`");
}

/// FIX ROUND 1 MIRROR (A1.2) — the drive STOPS AT the first lethal cycle. It does not commit
/// `n` cycles blindly and reconcile the deaths afterwards.
///
/// # Why the offer has to be doctored, and why that is the honest construction
///
/// A within-bound count can NEVER cross a CR 704.5a threshold: `elimination_bounds` narrows to
/// `min over living seats of (life - 1) / per-cycle loss` with FLOOR division, so `n * loss <=
/// life - 1` for every seat and every legal `n`. MEASURED at the bound on both fixtures after
/// this round's fix — bloodloop3 `n = 16` lands `[20, 1, 1]`, dina `n = 30` lands
/// `[79, 5, 1, 6]`, zero eliminations in both. The bounded class therefore cannot reach its
/// own cross-lethal arm through an undoctored offer, and a mirror row built on one would be
/// unbuildable rather than merely weak.
///
/// So this row is a HOSTILE fixture: it widens `schema.max_iterations` on the offer the engine
/// wrote — simulating a producer whose bound is WRONG — and then declares a count that arithmetic
/// says must kill. Everything downstream is production: `apply()`'s declare handler, the APNAP
/// window, `apply_confirmed_shortcut`, `materialize_fixed_shortcut`. The question it answers is
/// the one that matters when a certificate is unsound: does the drive stop at the boundary, or
/// does it drive through it?
///
/// CR 704.3: state-based actions are checked whenever a player would get priority, and the
/// drive's every beat goes through `pass_priority_once_with_pipeline`, so CR 704.5a ("if a
/// player has 0 or less life, that player loses the game") is applied INSIDE the drive.
///
/// # SCOPE — this row covers the TOTAL-WIPE arm ONLY (fix round 2, MED-1)
///
/// bloodloop3 seats its two opponents at EQUAL life (17/17 at the offer beat, measured), so they
/// cross 0 on the SAME cycle, CR 104.2a crowns, and the drive takes `CycleOutcome::CrossLethal`.
/// The fixture is structurally incapable of a partial wipe: a symmetric fixture collapses every
/// partial case into a total case. The other arm — one seat crosses while ≥2 players survive, no
/// `GameOver`, `CycleOutcome::Abort`, the crossing cycle rolling back whole while prior
/// conforming cycles stay committed — behaves DIFFERENTLY
/// and has its own row, [`bounded_fixed_drive_rolls_back_a_partial_crossing_cycle`], which
/// carries the arm-asymmetry table. Both arms are out of contract for any legitimately-derived
/// bound; each is reachable only under a doctored one.
///
/// # The MATCHED PAIR, on the same doctored offer
///
/// * ⓐ `n = cycles_to_lethal - 1` — the drive runs to completion, every seat survives at
///   exactly one point of life, nobody is eliminated.
/// * ⓑ `n = 2 * cycles_to_lethal` — the drive stops at the FIRST crossing cycle.
///
/// Without ⓐ, ⓑ alone is satisfied by a materializer that ignores `n` entirely and simply runs
/// the loop until something dies — which is exactly what `c6d834040` did. ⓐ is what forces the
/// stop point to be `n`-sensitive.
///
/// ⚠ ⓐ's DOCTORING IS A NO-OP ON THIS FIXTURE, and that is stated rather than dressed up (fix
/// round 2, LOW-1). bloodloop3's honest bound is 16 and `cycles_to_lethal - 1 = 17 - 1 = 16`, so
/// `schema.max_iterations = survivor_n` writes back the value already present — asserted below,
/// so a fixture drift cannot silently turn it into a real widening. ⓐ is therefore an
/// AT-THE-BOUND instance of [`bounded_fixed_count_commits_exactly_n_periods`], not an
/// independent stop-short observation. The pair's stop-short content rests entirely on ⓑ's
/// clause (b).
///
/// # What flips
///
/// * delete the frame delimiter from `drive_one_shortcut_cycle` ⇒ arm ⓐ runs to lethal instead
///   of stopping at 16 periods ⇒ its zero-elimination assertion FAILS. (Arm ⓑ does NOT flip:
///   the unbounded HEAD drive coincidentally halts at the same lethal board. Stated so the
///   pair's discrimination is not overclaimed — ⓐ carries it.)
/// * a blind implementation that ran all `2 * cycles_to_lethal` periods and reconciled the
///   deaths afterwards would leave the opponents at `17 - 34 = -17`; ⓑ's (b) pins the stop
///   point to `ceil(life / loss)` periods, derived from the published δ, so an overshoot of
///   even one cycle FAILS.
#[test]
fn bounded_fixed_drive_stops_at_the_first_lethal_cycle() {
    let mut state = bloodloop_state(3);
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire at 3 players");

    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let bound = schema.max_iterations;
    let lives_before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();

    // Per-seat loss the offer published; the stop point is derived from it, never from a literal.
    let loss = |seat: &PlayerId| -per_cycle.delta.life.get(seat).copied().unwrap_or(0);
    let victims: Vec<PlayerId> = lives_before
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| loss(id) > 0)
        .collect();
    assert_eq!(
        victims.len(),
        2,
        "REACH-GUARD: this row is about a MULTI-seat partial wipe, so both opponents must be \
         losing life per period; published δ {:?}",
        per_cycle.delta.life
    );

    // The smallest count that drives some living seat to 0 or less: `ceil(life / loss)`.
    let cycles_to_lethal = lives_before
        .iter()
        .filter(|(id, _)| loss(id) > 0)
        .map(|(id, l0)| l0.div_euclid(loss(id)) + i64::from(l0.rem_euclid(loss(id)) != 0))
        .min()
        .expect("at least one seat is losing life, asserted above");
    let n = u32::try_from(cycles_to_lethal).expect("fits") * 2;
    assert!(
        i64::from(n) > cycles_to_lethal,
        "REACH-GUARD: `n` must be COMFORTABLY past the first lethal cycle, else 'stops at the \
         boundary' and 'ran to completion' are the same observation"
    );
    assert!(
        n > bound,
        "REACH-GUARD: a lethal `n` is by construction above the honest bound ({bound}) — that \
         is the contract this row is deliberately violating to test the drive's own behaviour"
    );

    // ⓐ SURVIVING ARM — one period short of the first crossing. Same doctored offer, so the
    //   only difference between the arms is `n` itself.
    {
        let mut survive = state.clone();
        let survivor_n = u32::try_from(cycles_to_lethal - 1).expect("fits");
        let WaitingFor::LoopShortcut { schema, .. } = &mut survive.waiting_for else {
            unreachable!("bounded_offer_parts already matched the offer")
        };
        // The no-op recorded in this row's doc, pinned so it cannot drift unnoticed: on THIS
        // fixture the honest bound already equals `cycles_to_lethal - 1`, so the line below
        // rewrites the value in place. If a fixture change ever makes them differ, ⓐ becomes a
        // genuine doctored widening and its doc must be re-derived rather than re-read.
        assert_eq!(
            schema.max_iterations, survivor_n,
            "ⓐ's assignment is a NO-OP on this fixture (honest bound == cycles_to_lethal - 1); \
             a divergence means ⓐ is no longer an at-the-bound instance"
        );
        schema.max_iterations = survivor_n;
        r6a_declare_and_accept_all(&mut survive, proposer, survivor_n);
        assert_eq!(
            survive.players.iter().filter(|p| p.is_eliminated).count(),
            0,
            "ⓐ CR 704.5a: one period short of the crossing, every seat is still above 0; \
             lives {:?}",
            survive.players.iter().map(|p| p.life).collect::<Vec<_>>()
        );
        for (seat, l0) in &lives_before {
            let life_now = survive.players.iter().find(|p| p.id == *seat).unwrap().life as i64;
            assert_eq!(
                l0 - life_now,
                i64::from(survivor_n) * loss(seat),
                "ⓐ {seat:?}: exactly `n` periods committed, no more — the drive must stop \
                 because `n` ran out, not because something died"
            );
        }
        assert!(
            matches!(survive.waiting_for, WaitingFor::Priority { .. }),
            "ⓐ a completed finite drive hands back to priority; got {:?}",
            survive.waiting_for
        );
    }

    // ⓑ the doctoring, and ONLY this ──
    let WaitingFor::LoopShortcut { schema, .. } = &mut state.waiting_for else {
        unreachable!("bounded_offer_parts already matched the offer")
    };
    schema.max_iterations = n;

    r6a_declare_and_accept_all(&mut state, proposer, n);

    // (a) CR 704.3 + CR 704.5a: the drive stopped at a terminal state applied INSIDE it.
    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver {
            winner: Some(proposer)
        },
        "CR 704.5a: with every opponent at 0 or less life, CR 104.2a crowns the last player \
         standing, and the drive commits + stops there"
    );
    let eliminated: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| p.is_eliminated)
        .map(|p| p.id)
        .collect();

    // (c) EXACTLY the seats the published period drains — never a full-`n` overshoot that takes
    //     the proposer down too, and never a subset that leaves a drained seat alive.
    //
    //     ⚠ THIS CLAIM IS PER-ARM (fix round 2, MED-1). It holds on the `CycleOutcome::
    //     CrossLethal` arm, which is the only arm this symmetric fixture can reach: the crossing
    //     cycle COMMITS and the eliminated set is exactly the victims. On the `Abort` arm — one
    //     seat crosses while ≥2 survive — the eliminated set is EMPTY, and empty because the
    //     crossing cycle was rolled back whole, not because nobody crossed. Same surface
    //     reading, two different facts; conflating them is what let this doc ship a claim
    //     measurement contradicts. The Abort arm's own row is
    //     `bounded_fixed_drive_rolls_back_a_partial_crossing_cycle`.
    assert_eq!(
        eliminated,
        victims,
        "CR 704.5a: on the total-wipe (GameOver) arm the eliminated set is exactly the seats the \
         published period drains; lives {:?}",
        state.players.iter().map(|p| p.life).collect::<Vec<_>>()
    );

    // (b) STRICTLY LESS than `n` periods committed, and pinned to the FIRST crossing cycle.
    for (seat, l0) in &lives_before {
        let committed = l0 - state.players.iter().find(|p| p.id == *seat).unwrap().life as i64;
        let full_n = i64::from(n) * loss(seat);
        if loss(seat) > 0 {
            assert!(
                committed < full_n,
                "{seat:?}: a drive that ran all {n} periods would have committed {full_n}; it \
                 must stop at the CR 704.5a boundary instead, got {committed}"
            );
            assert_eq!(
                committed,
                cycles_to_lethal * loss(seat),
                "{seat:?}: the drive stops at the FIRST cycle that crosses the threshold — \
                 `ceil(life / loss)` periods, derived from the published δ, not one more"
            );
        }
    }
}

/// FIX ROUND 2 (MED-1) — THE OTHER LETHAL ARM. A crossing that eliminates ONE seat while
/// **≥2 players survive** raises no `GameOver`, so the drive does not cross-lethal: it ABORTS.
/// The crossing cycle rolls back whole, the cycles before it stay committed, and every seat is
/// still alive at handback.
///
/// # The arm asymmetry, stated so a future drive learns it from the doc and not by accident
///
/// | arm | trigger | outcome |
/// |---|---|---|
/// | **total wipe** | every remaining opponent crosses 0 on the same cycle ⇒ `WaitingFor::GameOver` | `CycleOutcome::CrossLethal` — **the crossing cycle COMMITS**, the game ends |
/// | **partial crossing** | one seat crosses 0 while **≥2** players survive ⇒ no `GameOver` | `CycleOutcome::Abort` — **the crossing cycle rolls back WHOLE; prior conforming cycles STAY COMMITTED**; priority handback |
///
/// Both arms are **out of contract for any legitimately-derived bound**. `elimination_bounds`
/// narrows to `min over living seats of (life - 1) / per-cycle loss` with FLOOR division, so
/// `n * loss <= life - 1` for every seat at every legal `n` and a within-bound drive can never
/// reach either arm. Each is therefore reachable only under a **doctored** bound — which is what
/// both this row and [`bounded_fixed_drive_stops_at_the_first_lethal_cycle`] construct.
///
/// The `Abort` is the DESIGNED behaviour and this row asserts it rather than a wish. The property
/// it buys is **no half-applied period, ever**: the out-of-contract cycle is refused ATOMICALLY,
/// while conforming work already done is NOT discarded. That is strictly better than a
/// whole-drive rollback — materializing a partial elimination would leave the remaining
/// repetitions bounded by a δ the board stops moving (the surviving seats' per-cycle drain
/// changes the moment a drain target leaves the game), and discarding the conforming prefix
/// would throw away cycles the table's own agreed bound covers. See
/// `materialize_fixed_shortcut`'s `CycleOutcome::Abort` arm.
///
/// MEASURED SHAPE of that split on this fixture: honest bound 30, doctored `n` at or past the
/// first crossing (31) ⇒ **30 periods committed**, cycle 30 refused, nobody eliminated. The
/// assertions below bind to exactly that: `first_crossing - 1` periods, not zero and not `n`.
///
/// # Why this row had to exist separately — the fixture-symmetry trap
///
/// [`bounded_fixed_drive_stops_at_the_first_lethal_cycle`] is the mirror for the same
/// stop-short property, but its bloodloop3 fixture seats **two opponents at equal life** (17/17,
/// measured), so they cross on the SAME cycle and it can only ever exhibit the total-wipe arm.
/// A symmetric fixture collapses every partial case into a total case; the partial arm — the one
/// real multiplayer boards take, since equal life totals are the exception — had no fixture at
/// all. This row's dina 4p dump is ASYMMETRIC by measurement (opponents at 35/31/36, all draining
/// 1 per period ⇒ first crossings 35/31/36), and the reach-guards below FAIL if that ever drifts
/// into symmetry, which is what stops this row from silently becoming a second copy of the mirror.
///
/// # What is asserted, and what is deliberately NOT
///
/// Every quantity is derived from the certificate the ENGINE published and the offer-beat board.
/// The row asserts the OBSERVABLE outcome: exactly `first_crossing - 1` periods committed, zero
/// eliminations, every seat above 0, handback to ordinary priority.
///
/// It does NOT assert "the conformance check never fired", because a conformance drop at the same
/// cycle index and an `Abort` at that index leave IDENTICAL final states — both `break 'cycles`
/// onto the same rollback. That distinction was settled by a REVERT-PROBE instead: deleting the
/// conformance check from `materialize_fixed_shortcut` leaves this row GREEN and unchanged, so
/// the stop is the `Abort`, not the conformance drop. Asserting it from the state would have been
/// an unfalsifiable claim.
///
/// # REVERT-PROBES
///
/// * delete `|| frames_per_period.is_some_and(|k| frames_this_cycle >= k)` from
///   `drive_one_shortcut_cycle` ⇒ the dina drive commits ZERO (`Abort` at cycle 0) ⇒ the
///   committed-delta `assert_eq!` FAILS.
/// * MUST-NOT-FLIP: deleting the conformance check leaves this row green (measured) — it is the
///   `Abort` arm, not the conformance arm.
#[test]
fn bounded_fixed_drive_rolls_back_a_partial_crossing_cycle() {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire on the 4p dump");

    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let honest_bound = schema.max_iterations;
    let lives_before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();
    let loss = |seat: &PlayerId| -per_cycle.delta.life.get(seat).copied().unwrap_or(0);

    // `ceil(life / loss)` — the first cycle index at which each drained seat crosses 0.
    let crossings: Vec<(PlayerId, i64)> = lives_before
        .iter()
        .filter(|(id, _)| loss(id) > 0)
        .map(|(id, l0)| {
            (
                *id,
                l0.div_euclid(loss(id)) + i64::from(l0.rem_euclid(loss(id)) != 0),
            )
        })
        .collect();
    assert!(
        crossings.len() >= 2,
        "REACH-GUARD: a PARTIAL wipe needs at least two drained seats, else 'one crosses while \
         others survive' is unconstructible; published δ {:?}",
        per_cycle.delta.life
    );
    let first_crossing = crossings
        .iter()
        .map(|(_, c)| *c)
        .min()
        .expect("at least two drained seats, asserted above");

    // ── THE ASYMMETRY REACH-GUARD. This is the guard the mirror row could not have had.
    let first_victims: Vec<PlayerId> = crossings
        .iter()
        .filter(|(_, c)| *c == first_crossing)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        first_victims.len(),
        1,
        "REACH-GUARD: this row is about the PARTIAL arm, so exactly ONE seat may cross first. \
         Equal crossings would make the whole table die together, take `CycleOutcome::CrossLethal` \
         and silently re-test the mirror row's total-wipe arm instead; crossings {crossings:?}"
    );
    let survivors = state.players.iter().filter(|p| !p.is_eliminated).count() - first_victims.len();
    assert!(
        survivors >= 2,
        "REACH-GUARD: with fewer than two survivors CR 104.2a crowns and `WaitingFor::GameOver` \
         routes the drive to the CrossLethal arm; got {survivors} survivors at the first crossing"
    );

    // The honest bound is exactly one period short of that crossing — the CR 704.5a headroom
    // term (`life - 1`) with floor division. Asserted, not assumed: it is what makes the
    // doctoring below a REAL widening rather than a re-write of the value already present.
    assert_eq!(
        i64::from(honest_bound),
        first_crossing - 1,
        "`elimination_bounds` reserves one point of headroom, so the honest bound sits one \
         period below the first crossing; bound {honest_bound}, crossings {crossings:?}"
    );

    // Three doctored bounds: at the crossing, and comfortably past it. All three must stop at
    // the same place — a drive that stopped `n`-relative rather than at the boundary would not.
    for over in [0u32, 3, 9] {
        let mut doctored = state.clone();
        let n = u32::try_from(first_crossing).expect("fits") + over;
        let WaitingFor::LoopShortcut { schema, .. } = &mut doctored.waiting_for else {
            unreachable!("bounded_offer_parts already matched the offer")
        };
        schema.max_iterations = n;

        r6a_declare_and_accept_all(&mut doctored, proposer, n);

        // (a) NOBODY is eliminated — by ROLLBACK, not because nobody crossed. `n >= first
        //     crossing` means the arithmetic says a seat must die; the drive refuses the cycle.
        assert_eq!(
            doctored
                .players
                .iter()
                .filter(|p| p.is_eliminated)
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            Vec::<PlayerId>::new(),
            "n={n}: the crossing cycle is rolled back whole, so the eliminated set is EMPTY — \
             which is a different fact from 'nobody crossed'; lives {:?}",
            doctored.players.iter().map(|p| p.life).collect::<Vec<_>>()
        );
        assert!(
            doctored.players.iter().all(|p| p.life > 0),
            "n={n}: every seat is above the CR 704.5a threshold; lives {:?}",
            doctored.players.iter().map(|p| p.life).collect::<Vec<_>>()
        );

        // (b) EXACTLY `first_crossing - 1` periods committed: every cycle before the crossing
        //     one, and none of it. Derived from the published δ, never from a literal.
        for (seat, l0) in &lives_before {
            let committed = l0
                - doctored
                    .players
                    .iter()
                    .find(|p| p.id == *seat)
                    .unwrap()
                    .life as i64;
            assert_eq!(
                committed,
                (first_crossing - 1) * -per_cycle.delta.life.get(seat).copied().unwrap_or(0),
                "n={n} {seat:?}: the drive commits every period up to the crossing cycle and \
                 rolls that one back; lives {:?}",
                doctored.players.iter().map(|p| p.life).collect::<Vec<_>>()
            );
        }

        // (c) NOT the CrossLethal arm. `GameOver` here would mean the partial crossing crowned
        //     someone, which is the confusion this row exists to keep separate.
        assert_eq!(
            doctored.waiting_for,
            WaitingFor::Priority { player: proposer },
            "n={n}: CR 104.2a — a player wins only once ALL their opponents have left, and this \
             crossing eliminates at most one of three, so there is no winner to crown and the \
             aborted drive hands back ordinary priority rather than ending the game"
        );

        // (d) R3-a's ABORT ARM — the drive-end seam is the CR 732.2a ending point for this
        //     entry path too, and it discards the detection window before handing back.
        //     MEASURED: this fixture enters that seam with a LIVE ring (`ring=16`), so the
        //     emptiness below is a CLEARED ring and not an absent one. Its journal is
        //     ALREADY empty there (`answers=0`) — the populated-journal half of the same
        //     seam is pinned on the f4 dump by
        //     `fantastic_four_bounded_loop::r3a_the_accepted_drive_ends_at_the_priority_point_with_the_window_cleared`,
        //     the only fixture measured reaching this seam with answers recorded.
        assert!(
            doctored.loop_detect_ring.is_empty(),
            "n={n}: CR 732.2a — the aborted drive ends at the priority handback with the \
             detection window DISCARDED, so a later beat re-detects genuinely instead of this \
             same `apply()` re-offering the interrupted loop; ring still carries {} sample(s)",
            doctored.loop_detect_ring.len()
        );
        assert_eq!(
            doctored.loop_answers_recorded(),
            0,
            "n={n}: CR 603.5 — the recorded `may` answers describe the window that just ended, \
             and the same seam drops them together with the ring. ⚠ FORWARD TRIPWIRE, not a \
             co-equal half of that claim: MEASURED non-discriminating on THIS fixture — under a \
             mutant neutering only the seam's `loop_answer_journal = None` this clause stays \
             green (the journal already reads 0 when this fixture reaches the seam) while the \
             f4 row fails `left: 3, right: 0`. It earns its place by failing if a future writer \
             ever populates the journal on this entry path and the seam stops clearing it; the \
             DISCRIMINATING statement of the journal half is the f4 row named above"
        );
    }
}

/// FIX ROUND 1 (HIGH-3) — the conformance check `PeriodicDelta`'s doc has always specified
/// ("so a bounded drive can check that each committed cycle actually conformed") and which
/// nothing implemented. A committed cycle whose measured resource delta differs from the
/// published signature is DROPPED WHOLE and the drive hands back to manual play.
///
/// # Why it is load-bearing rather than belt-and-braces
///
/// `elimination_bounds` divided the CR 704.5a headroom (`life - 1`) by `per_cycle.delta` to
/// produce the count the table agreed to. If a committed cycle moves a different amount, that
/// division no longer describes the drive, and the remaining repetitions can carry a seat past
/// the threshold INSIDE the proposal — the exact conditional action CR 732.2a forbids.
///
/// # The hostile fixture
///
/// The offer is real (the engine wrote it after 400 driven beats); ONE field is then doctored —
/// the published `per_cycle.delta` gains a life term for a seat the loop does not touch, which
/// no cycle can ever produce. Everything downstream is production: `apply()`'s declare handler,
/// the APNAP window, `apply_confirmed_shortcut`, `materialize_fixed_shortcut`.
///
/// # Non-vacuity — the paired positive control is arm ⓐ
///
/// ⓐ runs the SAME trajectory with the signature untouched and commits `n × δ`. Without it, ⓑ's
/// zero-delta observation would be indistinguishable from "the offer never fired" or "the drive
/// aborts on this fixture anyway" — which is precisely the shape the HEAD defect had.
///
/// REVERT-PROBE: delete the `if actual != pd.delta { break 'cycles; }` block in
/// `materialize_fixed_shortcut` ⇒ ⓑ commits `n × (real δ)` like ⓐ ⇒ ⓑ's zero-delta assertion
/// FAILS while ⓐ stays green.
#[test]
fn a_cycle_that_does_not_match_the_published_period_is_dropped() {
    let n: u32 = 2;

    // ⓐ POSITIVE CONTROL — untouched signature, same trajectory.
    let mut control = bloodloop_state(3);
    drive_to_bounded_offer(&mut control, 400).expect("the bounded offer must fire");
    let (committed_ok, per_cycle, _) = accept_bounded_fixed(&mut control, n);
    assert!(
        committed_ok.iter().any(|(_, d)| *d != 0),
        "REACH-GUARD: the undoctored drive must COMMIT something, else ⓑ's zero proves nothing \
         about the conformance check; got {committed_ok:?}"
    );

    // ⓑ the same offer with the published period made unproducible.
    let mut state = bloodloop_state(3);
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire");
    let (proposer, _, _) = bounded_offer_parts(&state);
    let stowaway = state
        .players
        .iter()
        .map(|p| p.id)
        .find(|id| per_cycle.delta.life.get(id).copied().unwrap_or(0) == 0)
        .expect("the cascade's own controller loses no life, so a zero-term seat exists");
    let WaitingFor::LoopShortcut { certificate, .. } = &mut state.waiting_for else {
        unreachable!("bounded_offer_parts already matched the offer")
    };
    certificate
        .per_cycle
        .as_mut()
        .expect("a bounded offer publishes its signature")
        .delta
        .life
        .insert(stowaway, -7);

    let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
    r6a_declare_and_accept_all(&mut state, proposer, n);

    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        lives_before,
        "CR 732.2a: no cycle can produce the doctored period, so the FIRST one is dropped whole \
         and ZERO life moves — a partial commit would mean the drive kept a cycle whose \
         magnitude the agreed bound was not computed from"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a non-conforming drive falls closed to manual play (CR 800.4a living seat), it does \
         not crown and does not wedge; got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.players.iter().filter(|p| p.is_eliminated).count(),
        0,
        "nothing was committed, so nothing crossed a CR 704.5a threshold"
    );
}

/// FIX ROUND 1 (MED-4) — the AI's bounded-declare candidate is GENERATED, LEGAL, and DRIVES.
///
/// The `if schema.points.is_empty() && schema.is_bounded()` block in
/// `ai_support::candidates` shipped with zero coverage: deleting it left the engine suite and
/// every `phase-ai` suite green. (Fix round 3, LOW-3: a bare "(4167)" stood here with neither a
/// runner nor a filter recorded beside it, so it named a shape nobody could reproduce; it is
/// deleted rather than re-dressed, exactly as the same count was at
/// `bounded_offer_conjunct_tests`' module doc. The reproducible claim is this row's own
/// REVERT-PROBE line below.) Its sibling one screen away
/// (`ai_collapse_candidate_is_clamped_to_the_accepted_bound`) sets the standard this row
/// mirrors — generate the candidate through the production generator, then `apply()` it.
///
/// Without that candidate an AI proposer at a bounded offer has exactly two options:
/// `UntilLethal`, which `handle_declare_shortcut` refuses outright against a bounded offer, and
/// `DeclineShortcut`. The block is the difference between an AI that can take this shortcut and
/// one that structurally cannot.
///
/// ⚠ RECORDED, NOT FIXED (a scope note, measured here): the block is gated on
/// `schema.points.is_empty()`. A TARGETED bounded offer publishes pins, so the AI gets no
/// accept candidate at all — `UntilLethal` is refused for bounded and the `Fixed` candidate
/// carries `template: None`, which fail-closes on published pins. A targeted bounded offer is
/// therefore AI-undeclarable today. That is a coverage gap in the candidate generator, not a
/// soundness bug (the AI declines, which is always legal), and it is left for its own round.
///
/// REVERT-PROBE: delete the `schema.points.is_empty() && schema.is_bounded()` block ⇒
/// assertion (2) FAILS (`Fixed(bound)` absent from the generated candidates).
#[test]
fn ai_bounded_declare_candidate_is_generated_legal_and_drives() {
    use engine::analysis::decision_template::IterationCount;

    let mut state = bloodloop_state(3);
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire at 3 players");
    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let bound = schema.max_iterations;

    // (1) reach-guards: this row is about the BOUNDED, UNTARGETED shape the block gates on.
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: an unbounded offer takes a different generator arm; bound = {bound}"
    );
    assert!(
        schema.points.is_empty(),
        "REACH-GUARD: the block is gated on an empty pin set; got {:?}",
        schema.points
    );

    // (2) the production generator offers it.
    let expected = GameAction::DeclareShortcut {
        count: IterationCount::Fixed(bound),
        template: None,
    };
    let candidates = engine::ai_support::legal_actions(&state);
    assert!(
        candidates.contains(&expected),
        "the AI must be able to declare the bounded offer's own count; got {candidates:?}"
    );

    // (3) ...and the reducer ACCEPTS it — which is what makes (2) load-bearing rather than a
    //     restatement of the generator. A refused declaration hands straight back to priority.
    let lives_before: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
    apply(&mut state, proposer, expected)
        .expect("the AI's generated candidate must be accepted by the reducer");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "CR 732.2b: an accepted declaration opens the APNAP response window; a handback to \
         Priority would mean the generator produced a count the engine refuses. got {:?}",
        state.waiting_for
    );

    // (4) ...and the accepted count DRIVES. Bound to the published period, never a literal.
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            &mut state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
    for (seat, l0) in state
        .players
        .iter()
        .map(|p| p.id)
        .zip(&lives_before)
        .collect::<Vec<_>>()
    {
        let now = state.players.iter().find(|p| p.id == seat).unwrap().life as i64;
        assert_eq!(
            now - l0,
            i64::from(bound) * per_cycle.delta.life.get(&seat).copied().unwrap_or(0),
            "{seat:?}: the AI-declared count commits exactly `max_iterations` copies of the \
             published period"
        );
    }
    assert_eq!(
        state.players.iter().filter(|p| p.is_eliminated).count(),
        0,
        "CR 704.5a: the offered bound reserves `life - 1` of headroom, so the AI's own \
         maximal legal declaration still eliminates nobody"
    );
}

// ---------------------------------------------------------------------------
// G1 — THE VALIDATED RANGE MUST COVER THE DRIVEN RANGE (rows R6 / R7 / R9).
//
// INVARIANT: at declare time the firewall must validate the image of the selection function
// over the range the ACCEPTED COUNT will actually drive (`0..n`), against the offer's
// PUBLISHED `legal_targets`. Before this fix it validated `0..shortcut_drive_period(..)` — a
// range derived from the SCHEDULE's own length, which answers a different question — so it
// both ACCEPTED a pin whose driven image leaves the published set at an index the count
// reaches (arm A), and REFUSED conforming declarations whose count is shorter than the
// schedule (arms D1 and E).
//
// Every arm drives the PRODUCTION entry `apply_action(GameAction::DeclareShortcut { .. })`
// and asserts on the published `waiting_for`: `RespondToShortcut` = ingested (CR 732.2b's
// response window opened), `Priority` = refused into the manual-play handback.
// ---------------------------------------------------------------------------

/// Two objects on a 3p board, and the declare-time verdict for one (published set, count,
/// schedule) triple. The board is real and the offer is planted, exactly as
/// `declare_illegal_pin_falls_back_legal_ingests` plants it — what is under test is the
/// declare firewall, not the detector that would otherwise mint the offer.
///
/// `max_iterations` is 1_000 (the un-narrowed global cap), so no arm below is refused by the
/// count cap instead of by the range: the cap and the bound are upstream conjuncts that would
/// otherwise dominate every verdict in the table.
fn g1_declare_verdict(
    publish_b: bool,
    count: IterationCount,
    schedule_of: &dyn Fn(&YieldTarget, &YieldTarget) -> TargetSchedule,
    pin_twice: bool,
) -> WaitingFor {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    let obj_a = scenario.add_creature(P0, "Schedule Target A", 1, 1).id();
    let obj_b = scenario.add_creature(P0, "Schedule Target B", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    let source_of = |id: ObjectId| YieldTarget::ThisObject {
        source_id: id,
        incarnation: None,
        trigger_description: None,
    };
    let (a, b) = (source_of(obj_a), source_of(obj_b));
    let slot = DecisionSlot {
        source: a.clone(),
        index: 0,
    };
    let mut legal_targets = vec![TargetRef::Object(obj_a)];
    if publish_b {
        legal_targets.push(TargetRef::Object(obj_b));
    }
    let schema = ShortcutDecisionSchema {
        iteration_count: count.clone(),
        // No narrowed CR 732.2a bound — `Default` carries the global cap.
        max_iterations: ShortcutDecisionSchema::default().max_iterations,
        points: vec![DecisionPoint {
            slot: slot.clone(),
            kind: DecisionPointKind::Targets {
                legal_targets,
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };
    let mut targets = vec![TargetPin::Scheduled(schedule_of(&a, &b))];
    if pin_twice {
        // E-neg: two pins against a `min_targets == max_targets == 1` point. The cardinality
        // check sits OUTSIDE the per-index loop, so it must still refuse at count 0.
        targets.push(TargetPin::Scheduled(TargetSchedule::Constant(obj_rank(
            a.clone(),
        ))));
    }
    let template = DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets { slot, targets }],
        replay: ReplayMode::Scheduled {
            count: count.clone(),
        },
        key: DecisionGroupKey::from_sources(std::slice::from_ref(&a), DecisionKind::LoopChoice),
    };

    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema,
        declaration: None,
    };
    runner
        .act(GameAction::DeclareShortcut {
            count,
            template: Some(template),
        })
        .expect("declare dispatch succeeds (a refusal is a manual handback, not an error)");
    runner.state().waiting_for.clone()
}

fn piecewise_a_then_b(a: &YieldTarget, b: &YieldTarget) -> TargetSchedule {
    TargetSchedule::Piecewise(vec![(0, obj_rank(a.clone())), (5, obj_rank(b.clone()))])
}

fn piecewise_b_then_a(a: &YieldTarget, b: &YieldTarget) -> TargetSchedule {
    TargetSchedule::Piecewise(vec![(0, obj_rank(b.clone())), (5, obj_rank(a.clone()))])
}

fn round_robin_a_b(a: &YieldTarget, b: &YieldTarget) -> TargetSchedule {
    TargetSchedule::RoundRobin(vec![obj_rank(a.clone()), obj_rank(b.clone())])
}

/// R6 arms A / B / C — the validated range must COVER the driven range.
///
/// * **A (the fix's positive, ⚠ behaviour change).** Publishes only A; the schedule switches
///   to the UNPUBLISHED B at index 5; the declared count is 8, so the drive reaches index 5.
///   Post-fix this is REFUSED. Pre-fix the validated range was the schedule length (2), so
///   indices 5..8 were never checked and the declaration was INGESTED — the soundness hole.
/// * **B (reach-guard).** The identical schedule at count 5 never reaches the switch, so it
///   is ingested. Without B, arm A would also pass under a firewall that rejected everything.
/// * **C (attribution control).** The identical count-8 declaration with B ALSO published is
///   ingested — so A's refusal is attributable to the PUBLISHED SET and not to the count, the
///   schedule, or the harness.
///
/// REVERT-PROBE: pass `shortcut_drive_period(Some(t))` again in place of
/// `shortcut_validated_range(&count, Some(t))` ⇒ arm A is ingested ⇒ FAILS (and D1 below
/// FAILS with it), while B and C do not move.
#[test]
fn declared_count_beyond_the_published_schedule_window_is_refused() {
    // A — the driven range reaches the unpublished arm.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(8), &piecewise_a_then_b, false),
            WaitingFor::Priority { .. }
        ),
        "CR 732.2a: a count that drives into an UNPUBLISHED schedule arm is not a sequence \
         that may be legally taken — refuse to manual play"
    );
    // B — reach-guard: the same schedule inside the published window is ingested.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(5), &piecewise_a_then_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "reach-guard: a count that never reaches the switch is a conforming declaration"
    );
    // C — attribution: publish the second arm and the count-8 declaration is fine.
    assert!(
        matches!(
            g1_declare_verdict(true, IterationCount::Fixed(8), &piecewise_a_then_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "attribution control: with BOTH arms published, count 8 is conforming — so A's \
         refusal is the published set and not the count"
    );
}

/// R7 arms D1 / D2 / D3 — the range is EXACTLY the driven range, not a padded one.
///
/// * **D1 (over-refusal fix, ⚠ behaviour change).** A `RoundRobin[A,B]` rotation with only A
///   published, declared at count 1: the drive touches index 0 only, which selects A. Post-fix
///   INGESTED. Pre-fix the schedule-derived period (2) forced index 1 — an index nothing
///   drives — to be validated, and the declaration was refused. This is the over-veto class.
/// * **D2 (must-not-flip).** The same rotation with BOTH arms published is ingested at count 1
///   AND at count 8 — the protected arm, pinned so the fix cannot be mistaken for "accept
///   more".
/// * **D3 (mandatory discriminating negative).** The same rotation with only A published at
///   count 2 DOES reach index 1 ⇒ refused. D3 is what separates D1 from "the validation was
///   deleted": under a deleted firewall D3 would be ingested.
#[test]
fn declared_count_shorter_than_the_rotation_is_not_over_refused() {
    // D1 — the fix's over-refusal half.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(1), &round_robin_a_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "CR 732.2a: a count of 1 drives index 0 only, which selects the PUBLISHED arm — \
         refusing it is the over-veto this fix removes"
    );
    // D2 — must-not-flip, both counts.
    for count in [IterationCount::Fixed(1), IterationCount::Fixed(8)] {
        assert!(
            matches!(
                g1_declare_verdict(true, count.clone(), &round_robin_a_b, false),
                WaitingFor::RespondToShortcut { .. }
            ),
            "a fully-published rotation is conforming at {count:?} — this arm must not move"
        );
    }
    // D3 — the discriminating negative at the first index the count DOES reach.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(2), &round_robin_a_b, false),
            WaitingFor::Priority { .. }
        ),
        "count 2 reaches index 1, which selects the UNPUBLISHED arm ⇒ refused. If this is \
         ingested, the firewall is gone rather than correctly ranged"
    );
}

/// R9 arms E / E-neg — `Fixed(0)` validates over an EMPTY range, and the firewall is STILL
/// live there.
///
/// CR 732.2b: a shortened proposal's new ending point is the first deviating choice, and
/// CR 732.2c makes taking the shortcut mandatory once accepted — so a zero-repetition
/// proposal must be representable AND validatable. The `.max(1)` floor validated index 0 of a
/// range nothing drives.
///
/// * **E (⚠ behaviour change).** `Piecewise[(0,B),(5,A)]` with only A published, at count 0.
///   Index 0 selects the UNPUBLISHED B — but nothing drives index 0, so post-fix this is
///   INGESTED. The template shape is load-bearing: under `RoundRobin[A,B]` index 0 selects the
///   PUBLISHED A, so the arm would pass before and after and its revert-probe could not fail.
/// * **E-neg (anti-"validation deleted" control at the SAME count).** Two pins against a
///   one-target point at the same count 0: the cardinality check sits outside the index loop,
///   so it must still refuse. Without E-neg, E would also pass under a firewall that had been
///   deleted outright.
///
/// REVERT-PROBE: restore the `.max(1)` floor ⇒ E FAILS and no other arm moves (every other
/// arm's range is already ≥ 1).
#[test]
fn a_zero_count_declaration_validates_over_an_empty_range_but_still_checks_cardinality() {
    // E — nothing is driven, so nothing is out of the published set.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(0), &piecewise_b_then_a, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "CR 732.2b/c: a zero-repetition proposal drives no index, so no index can leave the \
         published set — the floor was refusing a conforming declaration"
    );
    // E-neg — the firewall is still live at the same count.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(0), &piecewise_b_then_a, true),
            WaitingFor::Priority { .. }
        ),
        "the cardinality check is OUTSIDE the index loop: two pins against a one-target \
         point are refused even at count 0. If this is ingested, E passed because validation \
         was deleted rather than because the range is empty"
    );
}

// ═════════════ PR-7 Phase 5c, ITEM 2 — the kill-declared-target stop-short row ═════════════

/// Player-scope hexproof (CR 702.11c). The refuser is RULED to be
/// hexproof rather than phasing: hexproof makes the pinned seat illegal at the DRIVE's
/// spec-aware CR 608.2b re-validation (the `GameAction::SelectTargets` the injector submits),
/// which is the backstop layer no other 5c row exercises. Phasing would instead fail at
/// `resolve_target`'s EXISTENCE half and double-cover R1's seam.
/// A SYNTHETIC harness prop, deliberately NOT named after any printing: it exists only to be
/// the thing that makes the pinned seat illegal mid-window, and inventing a real card name for
/// non-verbatim text is the fabrication hazard CLAUDE.md's "verify the card, not just the rule"
/// principle warns about. Plan §12 scopes the verbatim-Oracle rule to the card under test; the
/// card under test here is the SANGUINE_BOND drain, whose text IS verbatim.
const HEXPROOF_GRANT: &str = "You have hexproof.";

const P3: PlayerId = PlayerId(3);

/// The R5 board — 4 seats, P0 running the escalating TARGETED drain
/// (`SANGUINE_BOND` × `BLOODTHIRSTY_CONQUEROR`), P1/P2/P3 at 1000 life so the drive never
/// crosses lethal inside the declared window.
///
/// FOUR SEATS, and that is the §6 reach-guard rather than padding: killing the pinned seat
/// must leave **at least two** other legal seats standing. A one-element surviving set cannot
/// witness "did not re-choose" — a retargeting engine would have exactly one place to go and
/// a stopped engine and a retargeting engine would be indistinguishable at the seat level.
///
/// Returns `(runner, sanguine_bond, hexproof_source, kickoff)`. The hexproof source starts in
/// P1's HAND, where its static does not function, so both arms share a byte-identical board
/// up to the moment the kill arm puts it onto the battlefield.
fn r5_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    for seat in 1..4u8 {
        scenario.with_life(PlayerId(seat), 1000);
    }
    let bond = scenario
        .add_creature_from_oracle(P0, "Sanguine Bond", 2, 2, SANGUINE_BOND)
        .id();
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR);
    let hexproof_src = scenario
        .add_creature_to_hand_from_oracle(P1, "Test Hexproof Source", 0, 4, HEXPROOF_GRANT)
        .id();
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, bond, hexproof_src, kickoff)
}

/// A `Fixed(count)` template pinning the Sanguine Bond trigger's `target opponent` to one
/// seat for every iteration. The slot's source is the Bond itself, so `slot_source_prompted`
/// matches the mid-drive `TriggerTargetSelection` the injector must answer.
fn r5_pin_template(slot: DecisionSlot, seat: PlayerId, count: u32) -> DecisionTemplate {
    let source = slot.source.clone();
    DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets {
            slot,
            targets: vec![TargetPin::Player(seat)],
        }],
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(count),
        },
        key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
    }
}

/// Reach the R5 board's own bounded `LoopShortcut` offer and return the runner parked on it
/// plus every seat's life at that instant.
///
/// The offer is the ENGINE's, read off `state.waiting_for` — never an out-of-band call to the
/// offer predicate, which would only prove the predicate agrees with itself.
fn r5_reach_offer() -> (GameRunner, DecisionSlot, ObjectId, ObjectId, Vec<i32>) {
    let (mut runner, bond, hexproof_src, kickoff) = r5_board();
    let _ = runner.cast(kickoff).target_player(P1).resolve();
    let WaitingFor::LoopShortcut {
        proposer, schema, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the 4p targeted drain must OFFER a LoopShortcut, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    // SHAPE PIN, re-derived: the Bond's `target opponent` trigger resolves across a
    // `TriggerTargetSelection` window, so the answer-beat sampling site announces its entry
    // and the offer publishes exactly ONE CR 608.2b `Targets` point for the Bond's own slot.
    // A drift here means the announced set changed shape and the callers must be re-derived,
    // not relaxed.
    //
    // LAYER ATTRIBUTION NO LONGER RESTS ON EMPTINESS, and no shared helper replaces it — each
    // caller measures the declare-time outcome ITSELF, on its own board. The kill arm of
    // `a_declared_target_made_illegal_mid_drive_stops_short_and_never_retargets` asserts
    // `WaitingFor::RespondToShortcut` immediately after its `DeclareShortcut` ("LAYER
    // ATTRIBUTION, half two"), which proves the declare firewall INGESTED that declaration and
    // therefore that the refusal it measures later is the DRIVE's; the r28 rows assert the
    // complementary declare-time refusal on their own staged schemas.
    assert_eq!(
        schema.points.len(),
        1,
        "the R5 offer publishes the Bond's re-aimable `Targets` slot and nothing else; got \
         {:?}",
        schema.points
    );
    assert!(
        matches!(
            schema.points[0].kind,
            DecisionPointKind::Targets {
                min_targets: 1,
                max_targets: 1,
                ..
            }
        ),
        "the published point is the Bond trigger's single-player target slot; got {:?}",
        schema.points[0].kind
    );
    // CR 732.2a: the ENGINE-issued slot is the pin authority. Hand-assembling one here
    // silently drifted from it (`incarnation: None` vs the published `Some(0)`), which
    // `validate_pins` then rejected — a test artefact, not an engine defect.
    let pinned_slot = schema.points[0].slot.clone();
    let lives = vec![
        life(&runner, P0),
        life(&runner, P1),
        life(&runner, P2),
        life(&runner, P3),
    ];
    (runner, pinned_slot, bond, hexproof_src, lives)
}

/// The per-cycle life the pinned seat loses, probed by an independent `Fixed(1)`
/// materialization of this same board (one recurrence = one full cycle). Mirrors
/// [`probe_drain_delta`]; nothing below is bound to a literal drain rate.
fn r5_probe_delta() -> i32 {
    let (mut runner, slot, _bond, _hexproof_src, l0) = r5_reach_offer();
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(r5_pin_template(slot.clone(), P1, 1)),
        })
        .expect("declare Fixed(1) with a Player pin");
    accept_all_opponents(&mut runner);
    let delta = l0[1] - life(&runner, P1);
    assert!(
        delta > 0,
        "Fixed(1) must materialize a nonzero drain cycle on the PINNED seat, got {delta}"
    );
    delta
}

/// ITEM 2 / R5 ⭐ — **a declared target made illegal mid-drive stops the drive short and is
/// NEVER re-chosen.** Governing ruling, ledgered verbatim: *stop-short/abort, never silently
/// re-choose or skip.*
///
/// # The seam, and why hexproof is the ruled refuser
///
/// A `Fixed(n)` drive re-resolves its template per cycle and answers each mid-cycle
/// `TriggerTargetSelection` through `inject_pinned_answer`, which submits the pinned value as
/// a real `GameAction::SelectTargets`. That submission is the **drive-time CR 608.2b
/// re-validation** — the backstop layer, one below the declare-time firewall. Hexproof
/// (CR 702.11c, "can't be the target of spells or abilities **your opponents control**": the
/// source is P0's permanent and the pinned seat is P0's opponent) makes the pinned seat
/// illegal exactly there and nowhere earlier. Phasing was rejected as the refuser precisely
/// because it fails one layer up, at `resolve_target`'s existence half, double-covering R1.
///
/// # Constructed-board deviation, DISCLOSED (constructibility-first)
///
/// This row is built on a constructed 4-seat board rather than on a dump fixture, which §6
/// licenses explicitly. Two measurements forced it, and both are INLINED here rather than
/// cited, because the probe archive that holds them is untracked and never ships: (i) the
/// real `dellian_emblem_conqueror_4p` dump — the only tracked fixture whose loop targets a
/// player — runs **309 beats to `GameOver` and raises no `LoopShortcut` at all** under the
/// generic dump driver, so it cannot host a declared drive; (ii) the refuser has to be
/// *introduced* on a named seat's battlefield mid-window, which is a board construction
/// whichever fixture carries it. The construction itself is the tracked one —
/// `declare_illegal_pin_falls_back_legal_ingests` builds its declare-seam board the same way.
///
/// # The four-assertion anti-retarget set (§6), each named at its assertion below
///
/// 1. the drive **stops short** — zero of `N` cycles commit, and `N * delta` is what the same
///    board commits with the refuser absent;
/// 2. **no retarget** — neither surviving legal seat is drained;
/// 3. **no silent skip** — the aborting cycle is not skipped-and-continued: no seat moves at
///    all, and the drain's mirror gain on P0 does not move either;
/// 4. **state coherent post-abort** — ring cleared, priority handed to a living seat, nobody
///    eliminated, the board still carries the refuser.
///
/// # Reach-guards (without these every assertion above is vacuous)
///
/// * the CLEAN arm is the positive control: the identical board with the hexproof source left in
///   hand drives all `N` cycles onto the pinned seat, so a `0` in the kill arm is the refuser
///   firing and not a dead harness;
/// * `player_has_hexproof(P1)` flips `false → true` across the move, so the setup cannot
///   silently no-op;
/// * **the surviving legal set has `len() >= 2`** — §6's own reach-guard. A one-element set
///   cannot witness "did not re-choose";
/// * the declare firewall **passed** (`RespondToShortcut` opened) and the offer publishes
///   **no points**, so the refusal is attributable to the drive and not to `validate_pins`.
///
/// # REVERT-PROBES — every claim below is a MEASUREMENT, with its result INLINED
///
/// Nothing here cites a log path: the probe archive is untracked and never ships, so the
/// measured values are reproduced in full instead.
///
/// The headline result is that the anti-retarget OUTCOME is defended by **AT LEAST THREE
/// independent production guards**, which is why no single-guard probe flips this row. Three
/// are named and measured below. The enumeration is deliberately open — a fourth, the
/// pre-drive `decision_template::resolve` re-check at the top of `materialize_fixed_shortcut`'s
/// `'cycles` loop, exists and simply is not engaged by THIS refuser (measured: it fires in
/// neither arm, which is exactly §6's reason for ruling hexproof over phasing).
///
/// * **GUARD 1 — the drive's per-slot CR 608.2b target-legality rejection**
///   (`ability_utils::validate_selected_slots_with_specs`, its "Illegal target selected" arm),
///   reached through `inject_pinned_answer`'s `GameAction::SelectTargets` submission. Measured
///   on the UNMUTATED tree, reached exactly ONCE, on the pinned seat, against a live-derived
///   legal set: `target=Player(P1) live_legal=[Player(P2), Player(P3)] would_reject=true` ⇒
///   `pinned_submit_ok=false` ⇒ `RecastAbort` ⇒ `CycleOutcome::Abort` ⇒ `break 'cycles` at
///   `i=0`. This is the layer this row claims to exercise, and it is provably reached.
/// * **GUARD 2 — the CR 732.2a per-cycle conformance check** in `materialize_fixed_shortcut`
///   (`actual != per_cycle.delta` ⇒ `break 'cycles`).
/// * **GUARD 3 — `inject_pinned_answer`'s fail-closed catch-all** (CR 732.2a "no conditional
///   actions": any prompt kind with no Stage-2 pin producer ⇒ `RecastAbort` ⇒
///   `CycleOutcome::Abort`).
///
/// * **RP-1**, the plan-named *"first legal target"* fallback in `inject_pinned_answer` (when
///   the pinned submission is refused, answer the prompt with the prompt's own first legal
///   target) ⇒ **this row still PASSES** (`1 passed; 0 failed`, `EXIT=0`). Not a hole in the
///   assertions — instrumented, the mutation *does* reach and *does* retarget: the prompt's
///   live legal set is `[P2, P3]` (P1 already dropped by the layer system),
///   `pinned_submit_ok=false`, `fallback=[Player(P2)]`, `fallback_ok=true`. The retargeted
///   cycle is then caught by GUARD 2: measured `break=CONFORMANCE i=0 actual={P0:+1, P2:-1}
///   expected={P0:+1, P1:-1}` ⇒ the divergent cycle is dropped whole.
/// * **SINGLE-GUARD PROBE on GUARD 1** — disable ONLY the per-slot legality rejection, leaving
///   GUARD 2 and CR 732.2a conformance fully intact ⇒ **this row still PASSES** (`1 passed`,
///   `EXIT=0`). Measured: the illegal pinned submission is then ACCEPTED
///   (`pinned_submit_ok=true`), the cycle never recurs, and the drive walks on to a
///   `DeclareAttackers` prompt that GUARD 3 fails closed on ⇒ `CycleOutcome::Abort` ⇒
///   `break 'cycles` at `i=0`, with **zero** conformance breaks.
/// * **RP-1b**, RP-1 *plus* GUARD 2 disabled ⇒ **FAILS at named assertion (2)**, the
///   anti-retarget assertion: `left: (997, 1000)  right: (1000, 1000)` (all `N = 3` cycles
///   committed onto P2), `EXIT=101`. This is the row's discrimination proof. It does not need
///   to touch GUARD 3: a successfully retargeted cycle RECURS, so the unpinned-prompt arm is
///   never reached on that path.
/// * **RP-2**, `CycleOutcome::Abort => continue 'cycles` ⇒ **measured NOT discriminating**
///   (`1 passed`, `EXIT=0`), disclosed rather than papered over. This row's refuser is
///   PERMANENT, so every later cycle aborts too and `committed` never advances. A real property
///   of a permanent refuser, not a gap in the assertions — a transient one is unconstructible
///   on this harness (no in-drive hook removes a static).
///
/// **Read the inertness correctly.** These assertions are insensitive to the removal of any ONE
/// guard, and that is a property of PRODUCTION'S REDUNDANCY, not a weakness of the row: the row
/// asserts the observable outcome (stop short, never retarget), and production defends that
/// outcome three ways over. The row IS a discriminator — it reaches GUARD 1 exactly once with
/// `would_reject=true` on the pinned seat against a live-derived legal set, and RP-1b flips it
/// at a named assertion. What it is not is a single-guard regression pin; no probe result here
/// should be read as claiming otherwise.
#[test]
fn a_declared_target_made_illegal_mid_drive_stops_short_and_never_retargets() {
    use engine::types::zones::Zone;

    const N: u32 = 3;
    let delta = r5_probe_delta();

    // ───────────────────────── CLEAN arm — the positive control ─────────────────────────
    // Identical board, hexproof source left in hand (its static does not function there), so the
    // ONLY difference from the kill arm is whether the refuser is on the battlefield.
    let (mut clean, clean_slot, _clean_bond, _clean_hexproof_src, clean_l0) = r5_reach_offer();
    clean
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(N),
            template: Some(r5_pin_template(clean_slot.clone(), P1, N)),
        })
        .expect("declare Fixed(N) with a Player pin");
    accept_all_opponents(&mut clean);
    assert_eq!(
        life(&clean, P1),
        clean_l0[1] - (N as i32) * delta,
        "control: with no refuser the drive commits EXACTLY N cycles onto the PINNED seat"
    );
    assert_eq!(
        (life(&clean, P2), life(&clean, P3)),
        (clean_l0[2], clean_l0[3]),
        "control: the pin, not the seat order, is what selects the drained seat"
    );

    // ───────────────────────────────── KILL arm ─────────────────────────────────────────
    let (mut runner, slot, bond, hexproof_src, l0) = r5_reach_offer();
    assert!(
        !engine::game::static_abilities::player_has_hexproof(runner.state(), P1),
        "setup anti-vacuity: the pinned seat must START without hexproof, or the kill below \
         changes nothing"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(N),
            template: Some(r5_pin_template(slot.clone(), P1, N)),
        })
        .expect("declare Fixed(N) with a Player pin");
    // LAYER ATTRIBUTION, half two: the declare-time firewall INGESTED this declaration. The
    // refusal measured below therefore happened at the drive, which is the whole point of
    // choosing hexproof over phasing as the refuser.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "the declare firewall must PASS — otherwise this row measures `validate_pins`, not \
         the drive's CR 608.2b backstop; got {:?}",
        runner.state().waiting_for
    );

    // THE KILL: the refuser arrives on the pinned seat's battlefield through the production
    // zone pipeline, after the declaration has been ingested and before the table's Accept.
    {
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            hexproof_src,
            Zone::Battlefield,
            &mut events,
        );
        // CR 613.1: the grant is a continuous effect — re-derive the board so the legality
        // reads below are taken against the post-kill layers rather than a stale cache.
        engine::game::layers::mark_layers_full(runner.state_mut());
        engine::game::layers::evaluate_layers(runner.state_mut());
    }
    assert!(
        engine::game::static_abilities::player_has_hexproof(runner.state(), P1),
        "setup anti-vacuity: the kill must actually land — a silently inert hexproof source would \
         make every assertion below pass for the wrong reason"
    );
    assert!(
        !engine::game::targeting::player_is_legal_target(runner.state(), P1, bond, P0),
        "CR 702.11c: the pinned seat must now be an ILLEGAL target of the Bond's ability"
    );
    // §6's REACH-GUARD, asserted as a count so a shrinking board fails loudly: after the kill
    // at least TWO other seats are still legal, so a retargeting engine has somewhere to go.
    let surviving_legal: Vec<PlayerId> = [P2, P3]
        .into_iter()
        .filter(|&seat| {
            engine::game::targeting::player_is_legal_target(runner.state(), seat, bond, P0)
        })
        .collect();
    assert!(
        surviving_legal.len() >= 2,
        "a 1-element surviving legal set cannot witness `did not re-choose`; got \
         {surviving_legal:?}"
    );

    accept_all_opponents(&mut runner);

    // (1) STOPS SHORT — zero of N cycles committed. Bound to the measured `delta` and to the
    //     control arm above, never to a literal: `N * delta > 0` is what a completing drive
    //     would have taken off the pinned seat.
    assert!(
        N >= 2 && delta > 0,
        "the stop-short claim needs a window longer than one cycle and a nonzero drain rate"
    );
    assert_eq!(
        life(&runner, P1),
        l0[1],
        "the pinned seat must lose NOTHING: the drive stopped at the first cycle whose \
         re-validation refused, and that cycle rolled back whole"
    );
    // (2) NO RETARGET — both seats that were measured LEGAL above are untouched.
    assert_eq!(
        (life(&runner, P2), life(&runner, P3)),
        (l0[2], l0[3]),
        "stop-short, never silently re-choose: neither surviving legal seat may be drained. \
         A `first legal target` fallback in the injector drains P2 here"
    );
    // (3) NO SILENT SKIP — the drain's mirror gain on the controller did not move either, so
    //     the drive did not skip the refused cycle and press on with the remaining ones.
    assert_eq!(
        life(&runner, P0),
        l0[0],
        "no silent skip: a drive that skipped the refused cycle and continued would still \
         have run the remaining cycles and moved the controller's mirror gain"
    );
    // (4) STATE COHERENT POST-ABORT.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the abort hands priority back to a living seat (manual fallback), not a wrong-crown \
         and not a stuck response window"
    );
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the ring is cleared on handback so the same apply() does not instantly re-offer"
    );
    assert!(
        [P0, P1, P2, P3]
            .into_iter()
            .all(|seat| !is_eliminated(&runner, seat)),
        "the table stays live — this row is about a refused target, not about anyone dying"
    );
    assert!(
        runner.state().battlefield.contains(&hexproof_src),
        "the roll-back is scoped to the DRIVE: the board change that made the pin illegal is \
         not undone by the abort"
    );
}

/// **Row R27 conjunct (a1) — THE SPLIT IS REAL AND THE TWO HALVES DIFFER.**
///
/// CR 104.4b + CR 732.2a: `LoopDetectSample` separates the equality *comparand* from the
/// shortcut *evaluable*. Every later conjunct of R27 (a2/a3/b/c, U3) asserts that a
/// period-touch consumer reads the un-normalized half; **all of them are false PASSes if
/// the two halves happen to hold the same thing.** This row is the BASE/POST discipline
/// applied to the split itself: it pins that a real production sample's halves DIFFER on
/// the axis that made the split necessary.
///
/// The axis is the object allocator. `normalize_for_loop` zeroes `next_object_id`
/// (`types/game_state.rs`, `clone.next_object_id = 0;`) while the live sample keeps it,
/// and `zones::create_object` allocates `ObjectId(state.next_object_id)` then
/// `state.objects.insert(id, obj)` — so evaluating a token creation against a normalized
/// frame allocates `ObjectId(0)` and REPLACES whatever object id 0 is, corrupting the map
/// the resolution runs on. That is the concrete defect the split exists to prevent.
///
/// **Revert-probe:** make `GameState::loop_detect_live_sample` return
/// `self.normalize_for_loop()` (i.e. collapse the split) ⇒ `live.next_object_id` becomes
/// `0` ⇒ the `assert_ne!` and the `live == pre_sample` assertion both FAIL. The row is
/// deliberately NOT sensitive to which half any consumer reads — that is (a2)/(a3)'s job —
/// so it stays the honest positive control while those arms are the subject.
///
/// Fixture: the tracked `dellian_emblem_conqueror_4p` dump, driven through the production
/// `apply()` path so the ring is populated by `record_loop_detect_sample` itself and not by
/// a hand-built fixture.
#[test]
fn a_recorded_loop_detect_sample_keeps_a_live_half_normalization_would_have_erased() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    ));
    let mut state = restore_dump(&json);

    // ── REACH-GUARDS. Without these the assertions below are vacuous.
    assert!(
        state.loop_detection.samples(),
        "reach-guard: the dump must load with a SAMPLING loop-detection mode, else no sample \
         is ever recorded; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring — every frame below was accumulated by \
         THIS drive through the production producer, not restored from the dump"
    );

    let pin = engine_live_opponents(&state, P0).first().copied();

    // Drive until the production sampler has recorded at least one sample, capturing the
    // live `next_object_id` observed at the beat immediately BEFORE the ring grew. That
    // pre-sample value is what the `live` half must have preserved.
    let mut witness: Option<(u64, u64, u64, u64)> = None;
    for _ in 0..400 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        let before = state.loop_detect_ring.len();
        let next_object_id_before = state.next_object_id;
        if dump_drive_one_beat(&mut state, pin).is_err() {
            break;
        }
        if state.loop_detect_ring.len() > before {
            let sample = state
                .loop_detect_ring
                .back()
                .expect("the ring just grew, so it has a back element");
            witness = Some((
                next_object_id_before,
                sample.live.next_object_id,
                sample.normalized.next_object_id,
                state.next_object_id,
            ));
            break;
        }
    }

    let (before_beat, live, normalized, after_beat) = witness.expect(
        "reach-guard: the drive must record at least one loop-detect sample, else this row \
         asserts about a ring that was never populated and passes vacuously",
    );

    // The whole point of the axis: it must be non-degenerate on this board, so that
    // `live != normalized` is a real inequality and not `0 != 0` dressed up, and so that
    // the lower bound below actually bites.
    assert!(
        before_beat > 0,
        "reach-guard: the allocator axis must be non-degenerate — a board that had allocated \
         ZERO objects makes the split unobservable on this axis and every assertion below \
         trivially true; got next_object_id = {before_beat}"
    );

    // ── THE CLAIM, both directions.
    assert_eq!(
        normalized, 0,
        "CR 104.4b: the comparand half is `normalize_for_loop()`d, which zeroes the volatile \
         monotonic allocator so two positions reached at different times can compare equal"
    );
    // The sampler runs at the POST-pipeline frame, so objects allocated earlier in the same
    // beat are already counted: the live half is bracketed by the beat's own endpoints
    // rather than equal to either. Collapsing the split drives it to 0, which is below
    // `before_beat` (> 0 by the reach-guard above) and so fails this bound.
    assert!(
        (before_beat..=after_beat).contains(&live),
        "CR 732.2a: the evaluable half is the beat un-normalized — it must carry the live \
         allocator cursor as of the post-pipeline frame the sampler runs at, i.e. inside \
         [{before_beat}, {after_beat}], because a shortcut's 'predictable results' are \
         evaluated by really resolving against it and `zones::create_object` allocates \
         `ObjectId(state.next_object_id)`; got {live}"
    );
    assert_ne!(
        live, normalized,
        "THE SPLIT IS REAL: if the two halves agreed on this axis, R27's later conjuncts \
         (a2)/(a3)/(b)/(c) — every one of which asserts that a period-touch consumer reads the \
         un-normalized half — would be satisfiable by a build in which the split does not exist"
    );
}

// ─────────── 5d U2 / R28 — the declared template's `owner` is ENGINE-BOUND ───────────

/// CR 732.2a: stage the live offer with an EMPTY point set, so arm (a″) can reach the
/// `!offer.schema.points.is_empty()` block's SKIPPED path. Counterpart to
/// [`r28_nonempty_schema_offer`]; `offer.proposer` still comes from the live
/// `WaitingFor::LoopShortcut`, which is the firewall's engine-issued comparand.
fn r28_empty_schema_offer(runner: &mut GameRunner) {
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema,
        declaration: _,
    } = runner.state().waiting_for.clone()
    else {
        panic!("staged from the live offer, never from thin air");
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema: ShortcutDecisionSchema {
            points: vec![],
            ..schema
        },
        // `None` is a RULING, not a default. Passing the live field through would stage the R5
        // board's measured `Some` declaration into a control whose whole purpose is to be
        // declaration-free — and it would contradict the invariant that an empty schema
        // publishes no declaration, at the very fixture that stages an empty schema.
        declaration: None,
    };
}

/// The engine-issued offer's own point set, hand-assembled to match `r5_pin_template`'s slot.
///
/// The R5 board's live offer publishes ONE `Targets` point whose `legal_targets` are minted
/// from the live board; this stages the same shape with a FIXED target list so the row is
/// insensitive to seat-population drift. `offer.proposer` — the firewall's engine-issued
/// comparand — still comes from `WaitingFor::LoopShortcut`, which is what the row is about.
fn r28_nonempty_schema_offer(runner: &mut GameRunner, slot: DecisionSlot) {
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema,
        declaration: _,
    } = runner.state().waiting_for.clone()
    else {
        panic!("staged from the live offer, never from thin air");
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema: ShortcutDecisionSchema {
            points: vec![DecisionPoint {
                slot,
                kind: DecisionPointKind::Targets {
                    legal_targets: vec![
                        TargetRef::Player(P1),
                        TargetRef::Player(P2),
                        TargetRef::Player(P3),
                    ],
                    min_targets: 1,
                    max_targets: 1,
                    ordered: false,
                },
            }],
            ..schema
        },
        // Same ruling as [`r28_empty_schema_offer`]: both helpers exist to stage `schema.points`
        // and NOTHING else, so the two must keep differing in exactly one field. Staging a live
        // `Some` here would add a second axis to a pair whose whole value is being one apart.
        declaration: None,
    };
}

/// R28 arms (a)/(a′) — **CR 732.2a + CR 603.5: a declaration whose `template.owner` names
/// another seat is refused AT DECLARE.**
///
/// `template.owner` arrives VERBATIM from the client (`GameAction::DeclareShortcut { template }`
/// is forwarded whole) and it is the comparand the drive's seat guard uses to decide whose
/// CR 603.5 choice a pin may answer. Without a declare-time binding to the engine-issued
/// `LoopShortcutOffer.proposer`, that guard compares an attacker-chosen value against itself:
/// a declaration carrying `owner: <other seat>` satisfies `*player != template.owner` exactly
/// when the prompt's recipient IS that other seat, and the proposer's pinned value is
/// dispatched as the other seat's `GameAction::DecideOptionalEffect`.
///
/// **(a′) is the reach-guard**: the byte-identical declaration with `owner = P0` builds the
/// proposal and opens APNAP, proving the fixture reaches the firewall and that (a) is keyed to
/// the `owner` axis rather than to `predictability_gate` / `validate_pins` / the count cap.
///
/// REVERT-PROBE: delete `if template.as_ref().is_some_and(|t| t.owner != offer.proposer) { .. }`
/// from `handle_declare_shortcut` ⇒ the wrong-owner declaration is accepted, a proposal is
/// built, APNAP opens ⇒ **(a) FLIPS TO FAIL** while (a′) stays green.
#[test]
fn r28_a_declared_template_owning_another_seat_is_refused_at_declare() {
    for hostile in [false, true] {
        let (mut runner, slot, _bond, _hexproof, _lives) = r5_reach_offer();
        r28_nonempty_schema_offer(&mut runner, slot.clone());
        let WaitingFor::LoopShortcut { schema, .. } = runner.state().waiting_for.clone() else {
            panic!("staged offer");
        };
        assert_eq!(
            schema.points.len(),
            1,
            "reach-guard: this arm runs on a NON-empty schema, so `predictability_gate` and \
             `validate_pins` really run and (a′) proves they PASS"
        );

        let mut template = r5_pin_template(slot.clone(), P1, 1);
        if hostile {
            template.owner = P1;
        }
        assert_eq!(
            template.owner,
            if hostile { P1 } else { P0 },
            "the two arms differ in exactly one field"
        );
        let before = runner.state().clone();
        let result = runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: Some(template),
            })
            .expect("the declaration is dispatched either way — refusal is a HANDBACK");

        if hostile {
            // (a) refused into the manual handback.
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "(a) CR 800.4a: a wrong-`owner` declaration hands priority back, got {:?}",
                runner.state().waiting_for
            );
            assert!(
                !matches!(
                    runner.state().waiting_for,
                    WaitingFor::RespondToShortcut { .. }
                ),
                "(a) no `ShortcutProposal` may be built"
            );
            assert!(
                result.events.is_empty(),
                "(a) `handle_declare_shortcut` pushes NO events at all, so this is an exact \
                 assertion rather than a wildcard: {:?}",
                result.events
            );
            assert_eq!(
                before.players.iter().map(|p| p.life).collect::<Vec<_>>(),
                runner
                    .state()
                    .players
                    .iter()
                    .map(|p| p.life)
                    .collect::<Vec<_>>(),
                "(a) nothing was driven"
            );
        } else {
            // (a′) the matched positive: the proposal IS built and APNAP opens.
            let WaitingFor::RespondToShortcut { proposal, .. } = &runner.state().waiting_for else {
                panic!(
                    "(a′) the honest declaration must open APNAP, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(
                proposal.template.as_ref().map(|t| t.owner),
                Some(P0),
                "(a′) the proposal carries the engine-bound owner"
            );
        }
    }
}

/// R28 arm (a″) — **the firewall's PLACEMENT, which no other arm can see.**
///
/// The firewall sits OUTSIDE `if !offer.schema.points.is_empty()`. On an EMPTY-schema offer
/// that block is skipped entirely, so a `Some(template)` declaration would otherwise reach the
/// proposal without passing any template validation at all — `predictability_gate` and
/// `validate_pins` both live inside it. Arms (a)/(a′) run on a non-empty schema and therefore
/// pass whether the firewall is inside the block or outside it.
///
/// ⚠ **DISCLOSED REACHABILITY DOWNGRADE.** This arm used to run on the R5 offer's OWN empty
/// schema — the empty-schema path was reached NATURALLY. It no longer is: the answer-beat
/// sampling site announces the Bond's trigger entry, so the LIVE schema now publishes one
/// `Targets` point (`r5_reach_offer` pins that shape). BOTH arms below therefore STAGE the
/// empty schema through `r28_empty_schema_offer`, the same idiom `r28_nonempty_schema_offer`
/// uses in the other direction. What survives the downgrade: `offer.proposer` — the firewall's
/// engine-issued comparand — is still the live one, and the matched positive accepts an honest
/// declaration on the SAME staged path, so the row still discriminates the firewall from "the
/// staged path refuses everything". What does NOT survive: the claim that a real board reaches
/// this path on its own. Treat that as unproven here until a fixture whose live offer publishes
/// nothing is added.
///
/// REVERT-PROBE: move the firewall INSIDE the `!offer.schema.points.is_empty()` block ⇒ the
/// wrong-owner declaration is accepted here ⇒ **(a″) FLIPS TO FAIL** while (a)/(a′) stay green.
#[test]
fn r28_a_the_owner_firewall_is_reached_on_an_empty_schema_offer_too() {
    // matched positive first: the empty-schema path DOES accept an honest declaration.
    let (mut runner, slot, _bond, _h, _l) = r5_reach_offer();
    r28_empty_schema_offer(&mut runner);
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(r5_pin_template(slot.clone(), P1, 1)),
        })
        .expect("declare");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "(a″) reach-guard: an EMPTY-schema offer accepts an owner-correct declaration, so the \
         refusal below is the firewall and not the empty-schema path refusing everything"
    );

    let (mut runner, slot, _bond, _h, _l) = r5_reach_offer();
    r28_empty_schema_offer(&mut runner);
    let mut template = r5_pin_template(slot.clone(), P1, 1);
    template.owner = P1;
    let result = runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(template),
        })
        .expect("dispatched");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "(a″) the firewall runs BEFORE the `!points.is_empty()` guard, so an empty-schema \
         offer is covered too, got {:?}",
        runner.state().waiting_for
    );
    assert!(result.events.is_empty(), "(a″) no events on the handback");
}

/// R28 arms (c)/(c′)/(c″) — **the RESTORE ingress, the only one the declare firewall cannot
/// see, closed at the single consumption chokepoint.**
///
/// `ShortcutProposal` is plainly serialized inside `GameState.waiting_for`, and the
/// untrusted-restore scrubber rewrites only the two PRE-CAST waits — so a persisted
/// `WaitingFor::RespondToShortcut` decodes with its template intact, having never run
/// `handle_declare_shortcut`. `apply_confirmed_shortcut` is the sole route into both drives,
/// which is why the re-validation conjunct joins ITS existing fail-closed guard.
///
/// Driven over BOTH trust branches, because `Deserialize for PersistedGameState` dispatches on
/// the presence of a top-level `"state"` key and only the RAW arm runs the scrubber. Asserting
/// one branch would leave the other untested — and (c″) below shows they really are different
/// code paths rather than one path asserted twice.
///
/// * **(c)** `t.owner = P1` on a `proposal.proposer = P0` proposal ⇒ Accept is refused into the
///   manual handback: priority to a living seat, ZERO cycles committed, no `GameOver`.
/// * **(c′)** MATCHED POSITIVE, byte-identical except that one field ⇒ the drive runs and
///   commits. This is the reach-guard proving the fixture reaches `apply_confirmed_shortcut` at
///   all, and that (c) is keyed to the `owner` axis rather than to `is_alive`, the count cap or
///   the conformance check.
/// * **(c″-Raw)** the scrubber RUNS on this branch and leaves the wait untouched — which is the
///   mechanism that makes (c) necessary. **(c″-Trusted)** is a DIFFERENT claim: the scrubber is
///   not on that path at all, so the arm asserts survival across the trusted envelope and
///   claims nothing about the scrubber.
///
/// REVERT-PROBE: delete the `|| proposal.template.as_ref().is_some_and(|t| t.owner !=
/// proposal.proposer)` conjunct from `apply_confirmed_shortcut`'s guard ⇒ the round-tripped
/// wrong-owner proposal drives and commits ⇒ **(c) FLIPS TO FAIL** on both branches, while
/// (c′) stays green (it never depended on the conjunct).
#[test]
fn r28_c_a_restored_proposal_with_a_foreign_template_owner_is_refused_at_consumption() {
    for hostile in [false, true] {
        for trusted in [false, true] {
            let label = format!("hostile={hostile} trusted={trusted}");
            let (mut runner, slot, _bond, _h, lives) = r5_reach_offer();
            runner
                .act(GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(1),
                    template: Some(r5_pin_template(slot.clone(), P1, 1)),
                })
                .expect("declare opens APNAP");

            // Tamper the persisted wait exactly as a hand-edited dump would, THEN round-trip.
            // The declare firewall has already run and passed on the honest value, so nothing
            // below can be attributed to it.
            let WaitingFor::RespondToShortcut { proposal, .. } =
                &mut runner.state_mut().waiting_for
            else {
                panic!("{label}: APNAP must be open");
            };
            let expected_owner = if hostile { P1 } else { P0 };
            proposal
                .template
                .as_mut()
                .expect("the declared template rode into the proposal")
                .owner = expected_owner;
            // MEASURED CONSTRAINT ON THIS INGRESS, applied to BOTH arms so they stay
            // byte-identical except `owner`: `ShortcutProposal.per_cycle` carries a
            // `PlayerId`-keyed resource map, and `PlayerId` cannot deserialize from a JSON
            // object KEY — so a persisted `RespondToShortcut` whose proposal carries a
            // per-cycle signature fails to decode with `invalid type: string "0", expected
            // u8`. That is a pre-existing serde asymmetry this change does not touch; its
            // consequence here is that ingress I3 is reachable only for `per_cycle: None`
            // proposals, which is exactly the shipped `Some(template)` population. The guard
            // under test does not read `per_cycle`, so nulling it costs the row nothing.

            // The TRUSTED arm must carry a real resolution-wire envelope, not a bare
            // `GameState` under a `"state"` key. Upstream #6933 made
            // `resolution_state_version` a required discriminator and gave only the
            // PersistedRaw ingress permission to stamp v1 onto a legacy payload; the
            // TrustedEnvelope ingress deliberately stamps nothing, because a trusted
            // snapshot is WRITTEN as a versioned envelope and must retain its declared
            // compatibility mode. `GameState`'s derived `Serialize` emits no such field,
            // so hand-wrapping it produced a payload the trusted path is right to refuse.
            // Building through `ResolutionStateWire` is what `TrustedGameStateEnvelope`'s
            // own `Serialize` does, so this arm now round-trips the shape production
            // writes instead of one only this test ever constructed.
            let payload = if trusted {
                let wire = engine::types::resolution::ResolutionStateWire::from_game_state(
                    runner.state().clone(),
                );
                serde_json::json!({ "state": serde_json::to_value(wire).expect("wire serializes") })
            } else {
                serde_json::to_value(runner.state()).expect("state serializes")
            };
            let restored: GameState =
                serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
                    .unwrap_or_else(|error| {
                        panic!("{label}: decodes through the production boundary: {error}")
                    })
                    .into_game_state();

            // (c″) — the wait and its tampered owner SURVIVE the decode. On the Raw branch the
            // scrubber ran and left it alone (its `semantic_owner` match names only the two
            // pre-cast waits); on the Trusted branch the scrubber is not on the path at all.
            let WaitingFor::RespondToShortcut { proposal, .. } = &restored.waiting_for else {
                panic!(
                    "{label}: (c\u{2033}) the restore must NOT drop the wait — otherwise (c) \
                     would pass by a different mechanism entirely; got {:?}",
                    restored.waiting_for
                );
            };
            assert_eq!(
                proposal.template.as_ref().map(|t| t.owner),
                Some(expected_owner),
                "{label}: (c\u{2033}) the tampered owner reaches `apply_confirmed_shortcut` \
                 unchanged"
            );
            assert_eq!(
                proposal.proposer, P0,
                "{label}: the proposer is engine state"
            );

            let mut restored_runner = GameRunner::from_state(restored);
            accept_all_opponents(&mut restored_runner);

            let after: Vec<i32> = restored_runner
                .state()
                .players
                .iter()
                .map(|p| p.life)
                .collect();
            if hostile {
                assert!(
                    matches!(
                        restored_runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ),
                    "{label}: (c) CR 800.4a manual handback, got {:?}",
                    restored_runner.state().waiting_for
                );
                assert_eq!(
                    after, lives,
                    "{label}: (c) ZERO cycles committed — the board is byte-equal on life"
                );
            } else {
                assert_ne!(
                    after, lives,
                    "{label}: (c\u{2032}) the honest proposal DRIVES — without this the \
                     hostile arm's `no delta` assertion is vacuous"
                );
            }
        }
    }
}

// ─────── AI1 — the AI's bounded-declare candidate withdraws on a 0→1 schema ───────

/// **AI1 — the generator's `Fixed(max)` candidate is keyed to the PUBLISHED PIN SET, measured
/// in BOTH directions on ONE board.**
///
/// CR 732.2a. `ai_support::candidates` emits `DeclareShortcut { count: Fixed(max_iterations),
/// template: None }` only `if schema.points.is_empty() && schema.is_bounded()`, because a
/// `template: None` declaration fail-closes against a published pin set — the engine would
/// ACCEPT it and then discard it, handing the search layer an action that looks legal and is
/// not.
///
/// The R5 offer is exactly the board that MOVED: it used to publish nothing (so the `Fixed`
/// candidate was emitted), and the answer-beat sampling site now announces the Bond's trigger
/// entry, so it publishes one `Targets` point. This row is the pin for that transition.
///
/// * **arm (a), the live board:** one published point, and the offer carries the engine's own
///   declaration for it ⇒ the `Fixed` candidate is emitted CARRYING THAT DECLARATION.
/// * **arm (b), the POSITIVE CONTROL, same board one field apart:** stage the schema's `points`
///   empty ([`r28_empty_schema_offer`]) ⇒ the `Fixed` candidate is emitted with `template: None`.
///   Without this arm, arm (a) would be satisfied by a generator that emitted `Fixed`
///   unconditionally.
///
/// ⚠ **ARM (a)'S PREVIOUS CLAIM WAS THE OPPOSITE, AND IT IS SUPERSEDED, NOT BROKEN.** As
/// `ai1_the_bounded_declare_candidate_withdraws_when_the_offer_publishes_a_pin` it asserted
/// `assert_eq!(live, vec![GameAction::DeclineShortcut])` — that a published pin set WITHDREW the
/// declare candidate, because the only declaration the generator could emit carried
/// `template: None` and would be accepted-then-discarded. item-4 C2b gives the generator the
/// offer's own declaration to carry, so the withdrawal is exactly the behaviour this commit
/// replaces, and the name had to stop saying "withdraws".
///
/// **ARM (b) IS BYTE-IDENTICAL AND THAT IS EARNED, NOT LUCK.** [`r28_empty_schema_offer`] is a
/// rest-less destructure plus a rebuild literal, so C2b had to CHOOSE a value for `declaration`
/// there; it passes `None`. Threading the live field through would stage this board's measured
/// `Some` declaration into a control that exists to be declaration-free, and arm (b)'s
/// `template: None` match would fail. See that helper's own comment.
///
/// Both arms read the ENGINE's candidate set through `legal_actions`, the same seam
/// `phase-ai`'s search calls, so this is not a re-implementation of the gate agreeing with
/// itself. The row is deliberately NOT `#[ignore]`d: the two pre-existing `phase-ai` bounded
/// rows are, and an ignored row reports `ok` while executing nothing.
#[test]
fn ai1_the_bounded_declare_candidate_carries_the_offers_own_pin_when_one_is_published() {
    // ── arm (a): the LIVE offer, which now publishes one point ──
    let (mut runner, _slot, _bond, _hexproof, _lives) = r5_reach_offer();
    let WaitingFor::LoopShortcut {
        schema,
        declaration,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("r5_reach_offer returns at the offer");
    };
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: the `Fixed` candidate is gated on `is_bounded()` as well, so an unbounded \
         offer would withhold it for the wrong reason"
    );
    assert_eq!(
        schema.points.len(),
        1,
        "REACH-GUARD: the published pin set is the conjunct this row is about; got {:?}",
        schema.points
    );
    let declaration = declaration.expect(
        "REACH-GUARD: this board's proposer answered its one published point, so the offer \
         publishes a declaration — without one arm (a) would measure the fail-closed path \
         `d6n_a_points_carrying_offer_without_a_declaration_enumerates_only_decline` covers",
    );
    let live = engine::ai_support::legal_actions(runner.state());
    assert_eq!(
        live,
        vec![
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(schema.max_iterations),
                template: Some(declaration),
            },
            GameAction::DeclineShortcut,
        ],
        "AI1(a): against a points-carrying bounded offer that HAS a declaration, the generator \
         emits it — carrying the ENGINE's own pin set, never one the AI built"
    );

    // ── arm (b): the POSITIVE CONTROL — the same board with an EMPTY point set ──
    r28_empty_schema_offer(&mut runner);
    let staged = engine::ai_support::legal_actions(runner.state());
    assert!(
        staged.iter().any(|a| matches!(
            a,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(_),
                template: None,
            }
        )),
        "AI1(b) POSITIVE CONTROL: with `points` empty the generator MUST emit the \
         `Fixed(max_iterations)` candidate again. Its absence here would mean arm (a) measured \
         a generator that emits nothing rather than one keyed to the pin set. got {staged:?}"
    );
    assert!(
        staged.contains(&GameAction::DeclineShortcut),
        "AI1(b): the decline stays legal on both arms — only the `Fixed` candidate moves, which \
         is what makes the pair one axis apart"
    );
}

/// **Row D7 — a PRE-DECLARATION save decodes with `declaration: None`, i.e. today's refusal.**
///
/// CR 732.2a. `WaitingFor::LoopShortcut.declaration` carries `#[serde(default)]`, following
/// `schema`'s precedent on the same variant. The consequence is CHOSEN, not discovered: a
/// snapshot written before this field existed decodes with `None`, the AI's declare candidate
/// stays withheld (`declaration.is_some()` is false) and the human path is unchanged — the same
/// behaviour that shipped before the field. Fail-closed by construction.
///
/// # Non-vacuity
///
/// The positive control is the round-trip WITH the key present: a decoder that always produced
/// `None` — or a `declaration` that never serialized at all — fails it. And the key's removal is
/// asserted to have actually removed something, so a typo in the field name cannot make the
/// "old save" arm pass by decoding an unmodified payload.
///
/// # ⚠ REVERT-PROBE, MEASURED — and the OBVIOUS probe is INERT, which is why it is named here
///
/// Deleting `#[serde(default)]` from the field does **NOT** red this row: measured, the stripped
/// payload still decodes and this test still passes. `serde_derive` routes a missing field
/// through `serde::__private::de::missing_field`, whose deserializer answers `deserialize_option`
/// with `visit_none` — so an `Option<T>` field is already missing-tolerant, and the attribute is
/// belt-and-braces here (it follows `schema`'s precedent on the same variant and states the
/// intent explicitly; it becomes load-bearing the moment the field stops being an `Option`).
///
/// The two probes that DO red this row, one per arm, both RUN:
///
/// * `#[serde(skip)]` in place of `#[serde(default)]` ⇒ the declaration never reaches the wire
///   ⇒ the POSITIVE CONTROL round-trip fails (a `Some(..)` decodes back as `None`);
/// * `#[serde(default = "…")]` pointing at a function returning `Some(..)` ⇒ the stripped
///   payload decodes with a fabricated declaration ⇒ the old-save arm's `matches!` fails.
#[test]
fn d7_a_pre_declaration_save_decodes_with_no_declaration() {
    let slot = DecisionSlot::target(YieldTarget::ThisObject {
        source_id: ObjectId(881),
        incarnation: Some(1),
        trigger_description: None,
    });
    let offer = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: None,
        certificate: synthetic_lethal_cert(),
        schema: ShortcutDecisionSchema {
            iteration_count: IterationCount::Fixed(3),
            max_iterations: 3,
            points: vec![DecisionPoint {
                slot: slot.clone(),
                kind: DecisionPointKind::Targets {
                    legal_targets: vec![TargetRef::Player(P1)],
                    min_targets: 1,
                    max_targets: 1,
                    ordered: false,
                },
            }],
            convoke_tappable_count: 0,
        },
        declaration: Some(DecisionTemplate {
            owner: P0,
            decisions: vec![PinnedDecision::Targets {
                slot: slot.clone(),
                targets: vec![TargetPin::Player(P1)],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(3),
            },
            key: DecisionGroupKey::from_sources(&[slot.source], DecisionKind::LoopChoice),
        }),
    };

    let mut json = serde_json::to_value(&offer).expect("the offer serializes");
    // POSITIVE CONTROL: with the key present the declaration survives the wire intact.
    assert_eq!(
        serde_json::from_value::<WaitingFor>(json.clone()).expect("round-trips"),
        offer,
        "a live offer's declaration must survive serialization — otherwise the `None` below \
         would prove nothing about the DEFAULT"
    );

    // The pre-C2b payload: the same offer with no `declaration` key at all.
    let removed = json["data"]
        .as_object_mut()
        .expect("the adjacently-tagged payload is an object")
        .remove("declaration");
    assert!(
        removed.is_some(),
        "reach-guard: the key must have been present to remove, else the 'old save' arm below \
         decodes an unmodified payload and asserts nothing"
    );
    let decoded: WaitingFor = serde_json::from_value(json).expect(
        "an OLD save must still decode (CR 732.2a offers \
             predate this field)",
    );
    assert!(
        matches!(
            decoded,
            WaitingFor::LoopShortcut {
                declaration: None,
                ..
            }
        ),
        "the forward-compatible default is `None`, which is today's refusal — fail-closed. got \
         {decoded:?}"
    );
}

// ───── PR #7005 maintainer item: the answer-beat sampler records the SYNCHRONIZED window ─────

/// CR 732.2a. `game::engine::apply_action`'s forced-window ANSWER sampler (the site gated on
/// `answering_forced_window`) called `record_loop_detect_sample` BEFORE installing the
/// pipeline's returned `wf`, while the settle sampler in `pass_priority_once_with_pipeline`
/// records AFTER its `sync_waiting_for`. A frame minted at the answer site therefore carried
/// the UN-SYNCED pair — whatever the reducer or `run_post_action_pipeline` last wrote straight
/// into `state.waiting_for`, with `priority_player` never recomputed — while a settle frame
/// carried the synced one. (NOT the "pre-pipeline" pair: the pipeline itself writes
/// `state.waiting_for` at five sites inside `run_post_action_pipeline_from`.)
///
/// That is a detection hazard, not cosmetics — but the consumer is BASIS A, not
/// `ring_delta_signature`. PR #7005's first commit and the comments it shipped said a
/// heterogeneous ring breaks `ring_delta_signature`'s turn-position conjunct "because
/// `impl PartialEq for GameState` compares both fields"; that is false at source. That
/// function reads only `ResourceVector::snapshot(&f.normalized)` and
/// `window_scope_from_cover_frames(..).phase_invariant` (= `turn_number` + `phase` +
/// `extra_phases.is_empty()`). The real sensitivity is the ring scans that call
/// `analysis::resource::loop_states_equal_modulo_resources(prior, state)` with `prior` a ring
/// frame's `normalized` half and `state` the live board: that chains to `loop_states_equal` ⇒
/// `impl PartialEq for GameState`, which DOES compare `waiting_for` and `priority_player`, and
/// neither `normalize_for_loop` nor `project_out_resources` neutralizes either — so an
/// un-synced frame compares UNEQUAL against a synced live board and basis A misses the
/// recurrence. The fix routes `wf` through `game::public_state::sync_waiting_for` — the
/// canonical synchronizer, which also recomputes `priority_player` via
/// `turn_control::authorized_submitter_for_player` — before the record, so both producers mint
/// the same shape.
///
/// FIXTURE: the tracked `dina_conqueror_4p` dump, driven through the production `apply()` path,
/// so every frame asserted below was minted by `record_loop_detect_sample` itself rather than
/// staged by the test.
///
/// SITE ATTRIBUTION IS DELIBERATELY NOT ATTEMPTED, and the paragraph that stood here claiming
/// it was "exact" was WRONG. It argued that because the settle sampler is reached only from
/// `pass_priority_once_with_pipeline` — i.e. from a `Priority` window, which
/// `is_forced_cascade_window` excludes — "the pre-beat window was forced AND the ring grew"
/// names the answer site and nothing else. That conflates the window BEFORE the beat with the
/// window at the settle sampler's own moment. One beat here is one `apply()`, which reaches the
/// settle sampler AFTER `apply_action` returns, so a beat whose pre-beat window was forced can
/// perfectly well mint at the SETTLE site — e.g. when answering it resolves the last stack
/// entry, gating the answer site off on `!stack.is_empty()` while the refill cascade settles.
/// `answered_forced_window` is ONE conjunct of the production answer-site gate (which also
/// demands `!in_simulation_probe()`, `loop_detection.samples()`, `!stack.is_empty()`, a
/// non-shrinking stack, and `Priority{player == active_player}`), so it is a REACH signal and
/// never an attribution.
///
/// The row therefore asserts arms (1) and (2) over EVERY frame a beat MINTED, which makes
/// attribution irrelevant rather than sharper: both samplers gate their record on
/// `Priority{player == active_player}` and both record after their own `sync_waiting_for`, so a
/// frame that fails either arm is a real defect whichever site minted it. The minted set is the
/// ring's pre-beat/post-beat `Arc`-identity MEMBERSHIP DIFFERENCE. No scalar is derived from the
/// ring's length, and the length-delta accounting that stood here is deleted rather than
/// sharpened — a scalar cannot name that set on either of the two production paths the loop body
/// documents. MEASURED on this drive: 2 forced-window minting beats (5, 14), 3 non-forced ones
/// (0, 9, 18), 5 frames minted and validated in total, offer at beat 19 over a 5-frame ring.
///
/// THIS FIXTURE REACHES NEITHER of the two paths that break a length delta, measured rather than
/// assumed: 0 evicting beats and 0 clear-and-rebuild beats over the drive, max ring 5 against a
/// capacity of 16. Both are reached — and the deleted scalar shown wrong on each — by
/// `an_evicting_beat_mints_without_growing_the_ring` and
/// `a_clearing_beat_rebuilds_the_ring_inside_the_same_beat`, on boards driven through the same
/// production `apply()`.
///
/// ⚠ WHAT THIS ROW DOES **NOT** CATCH, stated rather than implied. A PURE REVERT of the reorder
/// leaves all three arms GREEN, and that is a measurement rather than an oversight: an
/// instrumented `debug_assert_eq!` census on both fields at that position, run on the
/// pre-reorder tree over the full lib + integration corpus, reported 0 divergences (per-site
/// counts in PR #7005's history), so no fixture in the corpus reaches the divergence. The
/// row is consequently the STANDING pin — it fires the first time a beat does diverge — and its
/// instrument is proved live by MUTANTS at the sampler instead of by the revert:
/// * `state.priority_player = PlayerId(3);` after the sync ⇒ arm (2) FAILS at the first mint
///   (beat 5), `PlayerId(3)` vs `PlayerId(0)`, with arm (2)'s own message; arm (1) is never
///   reached.
/// * `state.waiting_for = WaitingFor::GameOver { winner: None };` after the sync ⇒ arm (1)
///   FAILS at the same beat, `GameOver { winner: None }` vs `Priority { player: PlayerId(0) }`,
///   with arm (1)'s own message. Arm (2) is SKIPPED there rather than passed — `GameOver` has
///   no acting player — which is exactly why arm (2) is an `if let` and not an unwrap: an
///   unwrap would panic on the `None` and replace arm (1)'s explanation with its own. Each
///   mutant is answered by a DIFFERENT arm, which is what "separately live" means.
///
/// Arm (3) is the BLAST-RADIUS pin: the certificate is byte-exact under the pure revert, which
/// is what makes "this reorder does not perturb detection" a measurement.
#[test]
fn answer_beat_frames_carry_the_synced_window_and_the_offer_certificate_is_exact() {
    use engine::analysis::resource::{PeriodicDelta, ResourceVector};

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));

    // ── REACH-GUARDS. Without these every assertion below is vacuous.
    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring, so neither a frame nor an \
         offer could exist; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring — every frame asserted below was \
         accumulated by THIS drive through the production producer, not restored from the dump"
    );

    let pin = engine_live_opponents(&state, P0).first().copied();
    let mut answer_mints = 0usize;
    let mut settle_mints = 0usize;
    let mut frames_validated = 0usize;
    let mut offer_beat = None;
    for beat in 0..400usize {
        if matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut {
                predicted_winner: None,
                ..
            }
        ) {
            offer_beat = Some(beat);
            break;
        }
        let answered_forced_window = state.waiting_for.is_forced_cascade_window();
        // THE BEAT'S MINTED SET IS A MEMBERSHIP DIFFERENCE. Nothing about the ring's LENGTH,
        // and nothing about its `back()`, can name it — both are unsound against production:
        //
        // * `GameState::record_loop_detect_sample` pops the front before pushing once the ring
        //   is at `LOOP_DETECT_RING_CAP`, so an evicting push mints while the length stays
        //   EQUAL. A `back()`-changed + `after - before` detector reads that as a mint of size
        //   zero and fails on a legitimate beat.
        // * `apply_action` clears the ring at its top for any non-`PassPriority`,
        //   non-`OrderTriggers` action answering a non-forced window, and the settle sampler in
        //   `pass_priority_once_with_pipeline` mints LATER in the same beat. Net growth is then
        //   `minted - cleared`, which is smaller than `minted` — a `take(net)` silently skips
        //   frames the row claims to have checked.
        //
        // The full pre-beat membership covers append, eviction and clear-and-rebuild uniformly,
        // and it makes the ring's capacity irrelevant to the row (no literal cap is transcribed
        // here; the const is private to `types::game_state`, so a copy could only rot).
        //
        // The snapshot holds `Arc` CLONES, not raw addresses, and that is load-bearing rather
        // than incidental: `pop_front` DROPS the evicted `Arc` before `Arc::new` allocates the
        // replacement, so an address-keyed snapshot can be aliased by the allocator handing the
        // freed block straight back, and a genuinely new frame would then read as an old one.
        // A retained strong reference makes every snapshotted address un-reusable for the whole
        // beat, so `Arc::ptr_eq` is exact by construction instead of by luck.
        let before: Vec<_> = state.loop_detect_ring.iter().cloned().collect();
        if dump_drive_one_beat(&mut state, pin).is_err() {
            break;
        }
        // Frames minted and then evicted WITHIN one beat are absent here by construction —
        // they are gone from the ring. "Every frame this beat added that the ring still holds"
        // is exactly the set the arms below claim, and exactly the set any consumer can read.
        let (minted, _) = ring_membership_delta(&before, &state.loop_detect_ring);
        if minted.is_empty() {
            continue;
        }
        frames_validated += minted.len();
        if answered_forced_window {
            answer_mints += 1;
        } else {
            settle_mints += minted.len();
        }
        // BOTH ARMS RUN OVER EVERY FRAME THIS BEAT MINTED, which is what makes site attribution
        // stop mattering — and the row deliberately does NOT try to attribute.
        //
        // `answered_forced_window` is `is_forced_cascade_window()` read BEFORE the beat, i.e.
        // strictly ONE conjunct of the production answer-site gate; that gate also demands
        // `!in_simulation_probe()`, `loop_detection.samples()`, `!stack.is_empty()`, a
        // non-shrinking stack, and `Priority{player == active_player}`. One beat here is one
        // `apply()`, which reaches the SETTLE sampler after `apply_action` returns. So a beat
        // that answers a forced window resolving the LAST stack entry gates the answer site
        // off (`!stack.is_empty()` false) while the refill cascade mints a settle frame, and
        // `answer_mints` increments over a frame the SETTLE site produced.
        //
        // Iterating the minted frames removes the question instead of sharpening it. Both
        // samplers gate their record on `Priority{player == active_player}` and both record
        // after their `sync_waiting_for`, so both arms hold for EITHER site's frame; a frame
        // that fails one is a real defect no matter which sampler produced it.
        for sample in minted {
            let frame = &sample.live;
            // ── (2) ITS PRIORITY PLAYER. Asserted FIRST so a mutation that touches only
            // `priority_player` is caught by its own arm instead of being masked by arm (1).
            //
            // The comparand is the AUTHORITY FUNCTION, not `frame.active_player`. `sync_waiting_for`
            // sets `priority_player = turn_control::authorized_submitter_for_player(state,
            // waiting_for.acting_player())`, which re-routes to a DIFFERENT seat whenever a
            // turn-decision controller (Mindslaver) or a latched search-decision controller is in
            // play. Pinning the seat itself would false-fail a correctly synced frame on any future
            // turn-control fixture. The recomputation is not circular: neither
            // `effective_authority_for_player` nor `search_decision_authority` reads
            // `priority_player`, so a mutant that clobbers only that field still fails here.
            //
            // `if let` rather than `expect`, and the difference is MEASURED: an `expect` here
            // pre-empts arm (1). A window with no acting player (`GameOver`) is precisely what
            // arm (1) exists to catch, so panicking on the `None` before reaching it replaces
            // arm (1)'s explanation with an unwrap message and destroys the arms' separation —
            // the `waiting_for = GameOver` mutant died on the unwrap instead of on arm (1). The
            // pair stays TOTAL, so this skip opens no hole: either arm (2) runs, or the window
            // had no actor and arm (1) below fails on that same frame.
            if let Some(semantic_player) = frame.waiting_for.acting_player() {
                assert_eq!(
                    frame.priority_player,
                    engine::game::turn_control::authorized_submitter_for_player(
                        frame,
                        semantic_player
                    ),
                    "beat {beat}: `sync_waiting_for` recomputes `priority_player` from the window \
                 it installs, so an answer-beat frame must carry that window's AUTHORIZED \
                 SUBMITTER, not whatever the un-synced state left behind"
                );
            }
            // ── (1) THE NEWEST SAMPLED STATE: the window the action RETURNS, never the forced one
            // it answered.
            assert_eq!(
                frame.waiting_for,
                WaitingFor::Priority {
                    player: frame.active_player
                },
                "beat {beat}: the sampler's own gate requires the RETURNED `wf` to be \
             `Priority{{active_player}}`, so recording before the sync is the only way the \
             frame can carry a different window — and `impl PartialEq for GameState` compares it"
            );
        }
    }

    assert!(
        answer_mints > 0,
        "reach-guard: the drive must reach at least one MINTING beat whose pre-beat window was \
         FORCED, else the answer-site path this row exists for was never exercised and it \
         passes vacuously. Stated exactly: this counts beats whose PRE-beat window satisfied \
         `is_forced_cascade_window()`, which is ONE conjunct of the production answer-site \
         gate, so it is a reach guard and NOT proof that the answer sampler is what minted; \
         got answer={answer_mints} settle={settle_mints}"
    );
    assert!(
        settle_mints > 0,
        "reach-guard for the WIDENING: arms (1)/(2) now run over every frame a beat minted, \
         from either sampler, so the drive must also mint at a non-forced (settle) beat — \
         otherwise the settle-frame coverage this row claims is untested. got \
         answer={answer_mints} settle={settle_mints}"
    );
    let ring_at_offer = state.loop_detect_ring.len();
    assert_eq!(
        frames_validated, ring_at_offer,
        "reach-guard on the MEMBERSHIP DIFFERENCE ITSELF, which is what makes 'every frame the \
         beat minted' a measurement instead of a claim: every frame the offer's ring holds must \
         have reached arms (1)/(2) as a member of some beat's minted set. A detector that \
         silently returned the EMPTY set on a minting beat — exactly what a length delta returns \
         at capacity — leaves frames in the ring that no arm ever read, and that lands here as \
         validated {frames_validated} against ring {ring_at_offer}. Equality rather than `>=` \
         because \
         this drive neither evicts nor clears (measured: max ring 5 against a capacity of 16, 0 \
         evicting and 0 clearing beats), so a frame counted but no longer present is equally a \
         defect on THIS board"
    );
    let offer_beat = offer_beat.expect(
        "reach-guard: the bounded offer must FIRE on this real 4p drain, else arm (3) asserts \
         about a certificate that was never published",
    );

    // ── (3) THE RESULTING CERTIFICATE, EXACT. The destructure is EXHAUSTIVE on purpose: a new
    // `LoopCertificate` field cannot slip past this pin unstated.
    let (proposer, certificate, _schema) = bounded_offer_parts(&state);
    assert_eq!(
        proposer, P0,
        "the offer beat {offer_beat} publishes the drain's controller as proposer"
    );
    let LoopCertificate {
        unbounded,
        win_kind,
        mandatory,
        residual_board_delta,
        per_cycle,
    } = certificate;
    assert_eq!(
        *unbounded,
        vec![
            ResourceAxis::Life(P0),
            ResourceAxis::Life(P1),
            ResourceAxis::Life(P2),
            ResourceAxis::Life(P3),
        ],
        "CR 119.3: the Dina/Conqueror drain moves EVERY seat's life each period — the three \
         opponents down and the controller up — so all four axes are unbounded"
    );
    assert_eq!(
        *win_kind,
        WinKind::LethalDamage,
        "CR 704.5a: opponents reach 0 life"
    );
    assert!(
        !*mandatory,
        "CR 732.2a: the interactive offer exists only for an OPTIONAL loop"
    );
    assert_eq!(
        *residual_board_delta,
        BoardDelta::default(),
        "CR 110.1: this cycle recycles its board exactly, so there is no non-recycled remainder"
    );
    let Some(PeriodicDelta {
        frames_per_period,
        delta,
        victim_slot,
    }) = per_cycle
    else {
        panic!(
            "the bounded producer is the one that NARROWS the CR 704 bound, so it publishes \
                a per-period signature; got None"
        )
    };
    assert_eq!(
        *frames_per_period, 2,
        "one repetition spans two retained ring frames on this board — the gain-life \
         resolution and the lose-life one"
    );
    assert!(
        victim_slot.is_empty(),
        "no decision slot is attributed a per-period life swing on this untargeted drain; \
         got {victim_slot:?}"
    );
    let mut expected_delta = ResourceVector::default();
    expected_delta.life.insert(P0, 1);
    expected_delta.life.insert(P1, -1);
    expected_delta.life.insert(P2, -1);
    expected_delta.life.insert(P3, -1);
    assert_eq!(
        *delta, expected_delta,
        "EXACT per-period signature: +1 to the controller, -1 to each opponent, and every \
         other axis at rest. `ring_delta_signature` is INSENSITIVE to \
         `waiting_for`/`priority_player` — it reads resource snapshots plus \
         `phase_invariant` (turn/phase/extra-phases) — so this arm is the blast-radius pin \
         for the reorder, not a restatement of arms (1)/(2). The frame homogeneity those two \
         arms pin is basis A's concern (`loop_states_equal_modulo_resources` ⇒ \
         `impl PartialEq for GameState`)"
    );
}

// ───── #7023 maintainer item: a beat's minted frames are a MEMBERSHIP set, not a length delta ─────

/// `(minted_frames, dropped)` for one beat, by `Arc` IDENTITY: frames the ring gained, and
/// pre-beat frames it lost. Generic over the sample type so the ring's private element type is
/// never named here.
///
/// `before` must be a slice of `Arc` CLONES held across the beat, not of raw addresses.
/// `GameState::record_loop_detect_sample` calls `pop_front()` — which DROPS the evicted
/// allocation — before `Arc::new` claims a new one of identical layout, so an address-keyed
/// snapshot can be aliased by the allocator handing the freed block straight back, and a
/// genuinely new frame would then read as an old one. A retained strong reference makes every
/// snapshotted address un-reusable for the whole beat, so `ptr_eq` is exact by construction
/// rather than by luck.
fn ring_membership_delta<'a, T>(
    before: &[std::sync::Arc<T>],
    after: &'a std::collections::VecDeque<std::sync::Arc<T>>,
) -> (Vec<&'a std::sync::Arc<T>>, usize) {
    let minted = after
        .iter()
        .filter(|f| !before.iter().any(|b| std::sync::Arc::ptr_eq(b, f)))
        .collect();
    let dropped = before
        .iter()
        .filter(|b| !after.iter().any(|f| std::sync::Arc::ptr_eq(b, f)))
        .count();
    (minted, dropped)
}

/// `dump_drive_one_beat`'s policy with its `Priority` arm taken directly. That policy is
/// unconditionally "pass" at a `Priority` window, so routing through the enumerator costs a full
/// per-viewer candidate scan — on the 152-entry `dellian` stack, the dominant cost of a long
/// drive — only to find the `PassPriority` the policy already chose. `apply` performs the real
/// legality check itself (`game::priority::pass_priority_legality`), so nothing is skipped but
/// the enumeration. Every other window still goes through the shared driver unchanged.
fn drive_one_beat_passing_fast(state: &mut GameState, pin: Option<PlayerId>) -> Result<(), String> {
    if let WaitingFor::Priority { player } = state.waiting_for {
        return apply(state, player, GameAction::PassPriority)
            .map(|_| ())
            .map_err(|e| format!("pass err: {e:?}"));
    }
    dump_drive_one_beat(state, pin).map(|_| ())
}

/// CR 732.2a. ROUTE ⓔ — EVICTION AT `LOOP_DETECT_RING_CAP`: a beat that MINTS WITHOUT GROWING.
///
/// `answer_beat_frames_carry_the_synced_window_and_the_offer_certificate_is_exact` reads a beat's
/// minted frames as the `loop_detect_ring`'s pre/post `Arc`-identity MEMBERSHIP DIFFERENCE. It
/// previously read them as `rev().take(after_len - before_len)` off a changed `back()`, and this
/// row plus its ⓒ sibling are the reachability half of the #7023 review that replaced that: the
/// scalar is wrong on two production routes, and the `dina_conqueror_4p` drive that row performs
/// reaches NEITHER — measured on it: max ring 5 against a capacity of 16, 0 evicting beats, 0
/// clearing beats. Correcting a detector against routes no fixture reaches would re-open the
/// evidential hole the correction exists to close, so each route gets a board.
///
/// THE MECHANISM. `GameState::record_loop_detect_sample` calls `pop_front()` and THEN
/// `push_back()` once the ring is at `LOOP_DETECT_RING_CAP`. One frame leaves, one arrives: the
/// back changes, the LENGTH DOES NOT. Net growth is 0 on a beat that minted 1, so
/// `rev().take(net)` inspects nothing and the deleted `assert!(grew >= 1, ..)` — labelled "fail
/// closed" in-tree — failed the test on an ordinary drain beat rather than on an anomaly. That
/// label was wrong about which side of the line the beat is on, and it went with the code.
///
/// FIXTURE: the tracked `dellian_emblem_conqueror_4p` dump, driven through production `apply()`,
/// so the ring that reaches capacity is this drive's own accumulation through
/// `record_loop_detect_sample`. MEASURED: first evicting beat at 72 (ring 16 -> 16, minted 1,
/// dropped 1), three more inside the first 90.
///
/// THE SEARCH PREDICATE IS STRUCTURAL AND THE ASSERTION IS THE CONSEQUENCE, never the reverse:
/// the witness is the first beat that LOST a pre-beat frame without the ring shrinking — pure
/// membership plus an ordering, saying nothing about minting — and the assertion is then what
/// the beat minted and what its length did. A witness selected on "minted while the length stood
/// still" would have carried its own conclusion into the arm that claims to test it.
///
/// This row does NOT re-assert the answer-beat row's frame invariants (the synced
/// `waiting_for`/`priority_player` pair). Its subject is the DETECTOR, not the frames.
#[test]
fn an_evicting_beat_mints_without_growing_the_ring() {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    )));
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring, so the capacity reached below is THIS \
         drive's accumulation through the production sampler and not a restored ring"
    );

    let pin = engine_live_opponents(&state, P0).first().copied();
    let mut max_ring = 0usize;
    let mut beats_run = 0usize;
    let mut evicting = None;
    for beat in 0..120usize {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        let before: Vec<_> = state.loop_detect_ring.iter().cloned().collect();
        if drive_one_beat_passing_fast(&mut state, pin).is_err() {
            break;
        }
        beats_run = beat + 1;
        max_ring = max_ring.max(state.loop_detect_ring.len());
        let (minted_frames, dropped) = ring_membership_delta(&before, &state.loop_detect_ring);
        let minted = minted_frames.len();
        if dropped == 0 || state.loop_detect_ring.len() < before.len() {
            continue;
        }
        evicting = Some((
            beat,
            before.len(),
            state.loop_detect_ring.len(),
            minted,
            dropped,
        ));
        break;
    }

    let (beat, before_len, after_len, minted, dropped) = evicting.unwrap_or_else(|| {
        panic!(
            "reach-guard: no beat replaced a ring frame without shrinking the ring, so this \
             drive never reached `LOOP_DETECT_RING_CAP` and the eviction route is untested — the \
             state the `dina_conqueror_4p` drive is permanently in, and the reason this row \
             exists on a different board. mode {:?}, max ring {max_ring} over {beats_run} beats",
            state.loop_detection
        )
    });
    assert_eq!(
        (minted, after_len),
        (1, before_len),
        "beat {beat}: an evicting push is 1-for-1 — `record_loop_detect_sample` pops the front \
         and THEN pushes at capacity — so this beat MINTED while its length stood still at \
         {before_len}, dropping {dropped}. THE DELETED SCALAR ON THIS BEAT: \
         `after_len - before_len` is 0, so `rev().take(grew)` inspects NOTHING while the beat \
         minted a frame, and `assert!(grew >= 1)` fails the whole test on an ordinary drain \
         beat. The membership difference reports the mint; no length delta can"
    );
}

/// CR 732.2a. ROUTE ⓒ — CLEAR-AND-REBUILD INSIDE ONE BEAT: a beat that MINTS MORE THAN IT GROWS.
///
/// Sibling of `an_evicting_beat_mints_without_growing_the_ring`; the shared argument for why both
/// routes need a board of their own is stated there.
///
/// THE MECHANISM. `game::engine::apply_action` clears the ring at its top for any action that is
/// neither `PassPriority` nor `OrderTriggers` answering a non-forced window.
/// `GameAction::SetAutoPass` at a `Priority` window is exactly that, and its own arm then calls
/// `pass_priority_once_with_pipeline`, whose settle sampler mints — after which `apply`'s
/// auto-pass loop can call it again inside the SAME beat. Net growth is therefore
/// `minted - cleared`, strictly below `minted` whenever the ring was non-empty, so `take(net)`
/// skips frames the answer-beat row claims to validate. MEASURED here: ring 2 -> 1, 2 dropped, 1
/// minted, i.e. net 0 against a real mint — the same blindness route ⓔ produces, reached the
/// other way, and the shallow-clear case (`0 < net < minted`) is the same defect with a smaller
/// margin.
///
/// `SetAutoPass { UntilStackEmpty }` IS NOT A TEST HOOK. It is the exact payload the client's
/// Arena-style "Resolve All" control dispatches (`client/src/game/dispatch.ts`), so this beat is
/// a player pressing that button mid-cascade. It is absent from
/// `ai_support::legal_actions_for_viewer`'s enumeration — `classify_flat_priority_action` files
/// it with the preference-propagation actions — which is why the generic dump driver never picks
/// it and why the route is dispatched by name here instead of being found by the driver's "first
/// legal action" policy. Dispatching it by name changes nothing about the beat: it is one
/// ordinary `apply()`, validated by the production reducer like any other.
///
/// FIXTURE: the tracked `dina_conqueror_4p` dump — the SAME board the answer-beat row drives, so
/// the contrast is exact. Left to itself that drive never clears (measured: 0 clearing beats);
/// one "Resolve All" press at the first `Priority` window carrying 2 accumulated frames puts it
/// on this route at beat 6.
///
/// SEARCH PREDICATE STRUCTURAL, ASSERTION THE CONSEQUENCE: the witness is the dispatched
/// `SetAutoPass` beat that lost EVERY pre-beat frame, and the assertion is what the beat minted
/// and what it left behind. The threshold of 2 accumulated frames is not cosmetic: below it a
/// wiped ring and a merely-evicted one are indistinguishable by membership.
#[test]
fn a_clearing_beat_rebuilds_the_ring_inside_the_same_beat() {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring, so the frames the clear below discards \
         were accumulated by THIS drive through the production sampler"
    );

    let pin = engine_live_opponents(&state, P0).first().copied();
    let mut fired = false;
    let mut max_ring = 0usize;
    let mut beats_run = 0usize;
    let mut clearing = None;
    for beat in 0..120usize {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        let before: Vec<_> = state.loop_detect_ring.iter().cloned().collect();
        max_ring = max_ring.max(before.len());
        let dispatched_here =
            !fired && before.len() >= 2 && matches!(state.waiting_for, WaitingFor::Priority { .. });
        let outcome = if dispatched_here {
            fired = true;
            let who = state
                .waiting_for
                .acting_player()
                .expect("a `Priority` window names its actor");
            apply(
                &mut state,
                who,
                GameAction::SetAutoPass {
                    mode: AutoPassRequest::UntilStackEmpty,
                },
            )
            .map(|_| ())
            .map_err(|e| format!("resolve-all err: {e:?}"))
        } else {
            drive_one_beat_passing_fast(&mut state, pin)
        };
        if outcome.is_err() {
            break;
        }
        beats_run = beat + 1;
        let (minted_frames, dropped) = ring_membership_delta(&before, &state.loop_detect_ring);
        let minted = minted_frames.len();
        if !dispatched_here || before.is_empty() || dropped != before.len() {
            continue;
        }
        clearing = Some((
            beat,
            before.len(),
            state.loop_detect_ring.len(),
            minted,
            dropped,
        ));
        break;
    }

    let (beat, before_len, after_len, minted, _dropped) = clearing.unwrap_or_else(|| {
        panic!(
            "reach-guard: no beat lost EVERY pre-beat ring frame, so `apply_action`'s \
             top-of-beat clear never ran on an accumulated ring and the clear-and-rebuild route \
             is untested. resolve-all dispatched={fired}, max ring {max_ring} over {beats_run} \
             beats"
        )
    });
    assert_eq!(
        (minted, after_len),
        (1, 1),
        "beat {beat}: `apply_action` cleared all {before_len} accumulated frames at the top of \
         this beat; a sampler must then have MINTED inside the SAME beat, leaving exactly one \
         frame — a left of `(0, 0)` means the beat only cleared, which is a different route and \
         proves nothing about the detector. THE DELETED SCALAR ON THE REBUILD BEAT: \
         `after_len - before_len` saturates to 0 against a real mint, so `take(grew)` validates \
         none of the frames the answer-beat row claims — and where the clear is shallower than \
         the rebuild the scalar is positive but still short. Net growth is `minted - cleared`; \
         it can never name `minted`"
    );
}

/// **Row T1b — STRUCTURAL, and the tier is FORCED.** CR 608.2b + CR 601.2c (reached via
/// CR 603.3d): BOTH `WaitingFor::TriggerTargetSelection` reducer arms route their
/// announcement through the single write authority `record_trigger_target_answer`.
///
/// # ⚠ WHY THIS IS A SOURCE CENSUS AND NOT A WIRE ROW — measured, not conceded
///
/// The `ChooseTarget` arm is covered end-to-end at the wire tier by
/// `fantastic_four_bounded_loop.rs`'s
/// `c2a_row_t1_the_announced_target_is_journalled_at_the_f4_offers_published_slot` and its
/// P2 provenance sibling. **The `SelectTargets` arm has NO tracked fixture that reaches it.**
/// Driving all five tracked 4p dumps in this file for 60 beats each through production
/// `apply()`: only `dellian_emblem_conqueror_4p` reaches a `TriggerTargetSelection` window,
/// it reaches seven of them, and every one enumerates `ChooseTarget` ×3 / `SelectTargets` ×0;
/// the other four reach none. See [`dump_drive_one_beat`]'s doc for the per-dump numbers.
/// A wire row for that arm is therefore not writable from this repo's fixtures today —
/// recorded as a BACKLOG item (needs a dump whose trigger declares a multi-slot or
/// object-target announcement), never as a silently-absent row.
///
/// This census covers exactly what it can: that the arm is WIRED. The writer's BEHAVIOUR is
/// proven separately and at a tier that can carry it — `game::engine`'s
/// `c2a_row_t5_an_unresolvable_target_abandons_the_whole_journal_write` drives the helper
/// itself, and both arms call that one helper, which is the point of it being one helper.
/// It is the same instrument class, and the same reasoning, as
/// `fantastic_four_bounded_loop.rs`'s ring-clear census.
///
/// # Discrimination
///
/// Delete the `record_trigger_target_answer(..)` call from EITHER arm ⇒ that arm lands in
/// `unwired` and this row reds NAMING the arm and its line. The mutation compiles (the other
/// caller survives), so it reds on the assert, not on a compile error. Without this row, r2's
/// own finding stands: deleting the `SelectTargets` call reds nothing in the suite.
///
/// # Reach-guard
///
/// The arm COUNT is asserted first and is independent of the call: a pattern reflow that hid
/// an arm from this scanner would read 1 or 0 and fail here rather than passing on a census
/// that found nothing to check.
#[test]
fn c2a_row_t1b_both_trigger_target_selection_arms_route_through_the_single_writer() {
    use std::path::Path;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("game/engine.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let lines: Vec<&str> = text.lines().collect();

    let (mut wired, mut unwired) = (Vec::new(), Vec::new());
    for (i, line) in lines.iter().enumerate() {
        let action = if line.contains("GameAction::SelectTargets {") {
            "SelectTargets"
        } else if line.contains("GameAction::ChooseTarget {") {
            "ChooseTarget"
        } else {
            continue;
        };
        // A reducer arm's `WaitingFor` half sits a few lines above its `GameAction` half in
        // the tuple pattern; anything further away is a different construct.
        if !lines[i.saturating_sub(10)..i]
            .join("\n")
            .contains("WaitingFor::TriggerTargetSelection {")
        {
            continue;
        }
        if lines[i..(i + 14).min(lines.len())]
            .join("\n")
            .contains("record_trigger_target_answer(")
        {
            wired.push(action);
        } else {
            unwired.push(format!("game/engine.rs:{} ({action})", i + 1));
        }
    }

    assert_eq!(
        wired.len() + unwired.len(),
        2,
        "reach-guard: `apply_action` has exactly TWO `WaitingFor::TriggerTargetSelection` \
         reducer arms (`SelectTargets` and `ChooseTarget`). A different count means an arm \
         was added, removed, or reflowed out of this scanner's reach — re-derive this census, \
         do not re-number it. wired={wired:?} unwired={unwired:?}"
    );
    assert!(
        unwired.is_empty(),
        "CR 608.2b: every `TriggerTargetSelection` reducer arm must journal its announcement \
         through `record_trigger_target_answer`, the single write authority. Unwired: \
         {unwired:?}"
    );
    assert_eq!(
        {
            let mut w = wired.clone();
            w.sort_unstable();
            w
        },
        vec!["ChooseTarget", "SelectTargets"],
        "both arms by NAME, not just by count: a census that found the same arm twice would \
         satisfy a bare count while leaving the other one unmeasured"
    );
}
