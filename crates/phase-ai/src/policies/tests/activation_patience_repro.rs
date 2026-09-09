//! Pin for [`crate::policies::activation_patience::ActivationPatiencePolicy`].
//!
//! Report (Discord #ai-suggestions): "The AI, in general, should not make blind
//! decisions during random phases, such as upkeep. If I have a 5-toughness
//! creature in play and my opponent has a Grim Lavamancer, there are very few
//! situations where the AI should activate the Grim Lavamancer to deal 2 damage
//! to my 5-toughness creature **without drawing a card first**. The only
//! situation where this makes sense is if the Grim Lavamancer is going to die or
//! leave play before the AI draws a card."
//!
//! # Why the fixture is Prodigal Sorcerer and not Grim Lavamancer
//!
//! Grim Lavamancer ("{R}, {T}, Exile two cards from your graveyard: This
//! creature deals 2 damage to any target", verified against
//! `data/card-data.json`) pays a **self-cost**, and `SelfCostValuePolicy`
//! already declines it against a board it cannot profitably shoot: exiling two
//! graveyard cards clears `REAL_COST_FLOOR`, and 2 damage that kills no opposing
//! creature and reaches no opponent's life total appraises as
//! `BenefitAppraisal::Trivial`. The reported card was therefore already covered.
//!
//! What was NOT covered is the same misplay with **no self-cost** to price —
//! Prodigal Sorcerer, "{T}: This creature deals 1 damage to any target"
//! (likewise verified). `self_cost_in_scope(AbilityCost::Tap)` is false, so that
//! gate never engages, and nothing else in the corpus modelled activation
//! timing: `TacticalWindow` has no upkeep variant and `card_hints` gives every
//! `ActivateAbility` a flat base score. Isolating on the uncovered shape is what
//! makes these tests discriminating rather than green-by-coincidence.
//!
//! # The arms
//!
//! One negative arm (hold at upkeep) and positive controls: one per escape
//! hatch (three), the phase gate, a deferrability check, and — added on review
//! (PR #8696) — a check that the draw step is correctly NOT gated. A policy
//! that simply always penalised would pass the negative arm alone.
//!
//! # Why the draw step is not one of the gated phases
//!
//! CR 117.3a and CR 504.1/504.2: the active player receives priority during
//! the draw step only AFTER the turn-based draw has been dealt with. So by the
//! time this predicate could ever see `Phase::Draw` with `WaitingFor::Priority`,
//! the card is already in hand — there is no more "wait to draw first"
//! information left to buy. `phase_draw_is_not_gated` below pins that; it is
//! the corrected reading of what was previously (incorrectly) one of the three
//! gated phases.

use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::policies::activation_patience::ActivationPatiencePolicy;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{PolicyVerdict, TacticalPolicy};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);
const OPP: PlayerId = PlayerId(1);

struct Ids(u64);

impl Ids {
    fn new() -> Self {
        Self(9300)
    }
    fn next(&mut self) -> CardId {
        self.0 += 1;
        CardId(self.0)
    }
}

fn creature(
    state: &mut GameState,
    ids: &mut Ids,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    oracle_text: Option<&str>,
) -> ObjectId {
    let id = create_object(
        state,
        ids.next(),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let parsed = oracle_text
        .map(|text| parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]).abilities);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.summoning_sick = false;
    if let Some(abilities) = parsed {
        *Arc::make_mut(&mut obj.abilities) = abilities;
    }
    id
}

struct Board {
    state: GameState,
    pinger: ObjectId,
}

/// The AI's own upkeep, empty stack: a pinger it could fire, an opposing wall it
/// cannot profitably shoot, and no reason on the board to act before the draw.
fn build_board(phase: Phase, ai_life: i32, opponent_life: i32) -> Board {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    state.phase = phase;
    state.active_player = AI;
    state.priority_player = AI;

    let pinger = creature(
        &mut state,
        &mut ids,
        AI,
        "Prodigal Sorcerer",
        1,
        1,
        Some("{T}: This creature deals 1 damage to any target."),
    );
    // The report's "generic 1/5": a body the ping cannot kill and that the AI
    // at a healthy life total can simply ignore.
    creature(&mut state, &mut ids, OPP, "Wall", 1, 5, None);

    state.players[AI.0 as usize].life = ai_life;
    state.players[OPP.0 as usize].life = opponent_life;
    state.waiting_for = WaitingFor::Priority { player: AI };
    Board { state, pinger }
}

fn verdict_for(board: &Board) -> PolicyVerdict {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session.features.insert(AI, Default::default());
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;

    let candidate = CandidateAction {
        action: GameAction::ActivateAbility {
            source_id: board.pinger,
            ability_index: 0,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    };
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: Vec::new(),
    };
    let ctx = PolicyContext {
        state: &board.state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
        search_depth: SearchDepth::Root,
    };
    ActivationPatiencePolicy.verdict(&ctx)
}

fn score_and_reason(verdict: &PolicyVerdict) -> (f64, &'static str) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (*delta, reason.kind),
        PolicyVerdict::Reject { reason } => {
            panic!(
                "patience is a soft policy and must never Reject, got {}",
                reason.kind
            )
        }
    }
}

/// The negative arm: a deferrable ability, the turn's lowest-information
/// window, and nothing on the board that makes waiting cost anything.
#[test]
fn a_deferrable_ping_is_held_through_the_ai_own_upkeep() {
    let board = build_board(Phase::Upkeep, 20, 20);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_hold");
    assert!(
        delta < 0.0,
        "firing a repeatable pinger before the draw step buys strictly less \
         information than waiting does, so it must be penalised — got {delta}"
    );
}

/// CR 505.1 / CR 506: the main phase is not a low-information window. The
/// policy must be silent there or it would become a blanket "never activate".
#[test]
fn the_main_phase_is_not_gated() {
    let board = build_board(Phase::PreCombatMain, 20, 20);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// Escape hatch: a lethal line. CR 104.3b — a player at 0 or less life loses,
/// so 1 damage into an opponent at 1 life is worth more than any information
/// another turn could buy.
#[test]
fn a_lethal_line_overrides_patience() {
    let board = build_board(Phase::Upkeep, 20, 1);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_lethal_line");
    assert_eq!(delta, 0.0);
}

/// Escape hatch: the report's own exception — "if the Grim Lavamancer is going
/// to die or leave play before the AI draws a card". Under real pressure
/// "wait" is not actually on offer, so the policy stands down.
#[test]
fn a_threatened_board_overrides_patience() {
    // Below `any_immediate_threat`'s 40%-of-starting-life floor.
    let board = build_board(Phase::Upkeep, 5, 20);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_threatened");
    assert_eq!(delta, 0.0);
}

/// A mana ability is not deferrable (CR 605.1a — it does not use the stack, and
/// CR 106.4 empties the pool at end of step), so the policy must not touch it.
/// This is what keeps the shared deferrability predicate honest.
#[test]
fn a_mana_ability_is_not_treated_as_deferrable() {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    state.phase = Phase::Upkeep;
    state.active_player = AI;
    state.priority_player = AI;
    let pinger = creature(
        &mut state,
        &mut ids,
        AI,
        "Llanowar Elves",
        1,
        1,
        Some("{T}: Add {G}."),
    );
    state.waiting_for = WaitingFor::Priority { player: AI };
    let board = Board { state, pinger };
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// Review finding (PR #8696): the module previously gated `Phase::Draw` on
/// the theory that it was one of the turn's low-information windows. It is
/// not — CR 117.3a / CR 504.1/504.2 mean the active player only receives
/// priority in the draw step AFTER the turn-based draw (and its triggers)
/// have been dealt with, so any draw-step priority is already post-draw. This
/// pins the corrected behavior: activating during the draw step must be
/// treated exactly like the main phase, not like upkeep.
#[test]
fn phase_draw_is_not_gated() {
    let board = build_board(Phase::Draw, 20, 20);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_na",
        "the draw step is post-draw by the time priority is offered (CR 504.1/504.2), so it \
         must not be treated as a low-information window"
    );
    assert_eq!(delta, 0.0);
}

/// Review finding (PR #8696): escape hatch 3 had no test, unlike its two
/// siblings, AND its CR citation was wrong twice over — first as CR 603.4
/// (a TRIGGERED ability's intervening-if), then, in a maintainer fixup, as
/// CR 602.5 ("a player can't begin to activate a PROHIBITED ability" —
/// summoning-sickness-gated tap costs, "activate only once each turn",
/// "activate only as a sorcery"). Neither describes this shape. Agadeem
/// Occultist's trailing "if" is a PAYOFF condition on an ACTIVATED ability,
/// evaluated at resolution per CR 608.2c — activating it is legal regardless
/// of whether the condition holds; only the effect can do nothing. This is
/// the exact class `condition_gated_activation.rs`'s own module doc already
/// describes correctly for hideaway lands: "the payoff ability IS legal to
/// activate under the threshold ... and it correctly does nothing at
/// resolution (CR 608.2c) when the condition is false." A regression deleting
/// the escape-hatch branch itself would still have passed this suite
/// silently, independent of the citation error — this test closes both gaps.
///
/// Agadeem Occultist's activated ability ("{T}: Put target creature card from
/// an opponent's graveyard onto the battlefield under your control if its mana
/// value is less than or equal to the number of Allies you control.", verified
/// against `data/card-data.json`) is a real, verified `{T}`-only conditional
/// activated ability — no self-cost, so `SelfCostValuePolicy` does not already
/// cover it, keeping this test isolated to the escape hatch under test.
#[test]
fn a_conditional_ability_overrides_patience() {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    state.phase = Phase::Upkeep;
    state.active_player = AI;
    state.priority_player = AI;

    let source = creature(
        &mut state,
        &mut ids,
        AI,
        "Agadeem Occultist",
        2,
        2,
        Some(
            "{T}: Put target creature card from an opponent's graveyard onto the battlefield \
             under your control if its mana value is less than or equal to the number of \
             Allies you control.",
        ),
    );
    assert!(
        state
            .objects
            .get(&source)
            .unwrap()
            .abilities
            .first()
            .is_some_and(|a| a.condition.is_some()),
        "premise of this test: the parser must produce a non-None `condition`"
    );

    state.waiting_for = WaitingFor::Priority { player: AI };
    let board = Board {
        state,
        pinger: source,
    };
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_conditional",
        "an activated ability's resolution-time payoff condition (CR 608.2c) must stand \
         down patience: activation is legal either way, but a condition TRUE now may not \
         still be true after waiting, risking the payoff for no real gain"
    );
    assert_eq!(delta, 0.0);
}

/// Review finding (PR #8696): `is_low_information_window` treated EVERY
/// empty-stack `Phase::Upkeep` priority state as the pristine "nothing has
/// happened yet" window. CR 503.1a queues "at the beginning of your upkeep"
/// triggers onto the stack BEFORE the active player's first priority grant —
/// so once such a trigger exists and resolves, priority returns to the SAME
/// shape (`Phase::Upkeep`, empty stack, `Priority`) a SECOND time, now
/// post-resolution. An unguarded predicate cannot tell the two apart, so an
/// information-producing trigger (Phyrexian Arena's "you draw a card and you
/// lose 1 life", verified against `data/card-data.json`) can resolve and the
/// policy still penalises the very next activation as though the AI "hasn't
/// drawn yet" — when, for that turn's extra card, it already has.
///
/// This does not attempt to distinguish the truly-first grant from a
/// post-resolution one (that needs engine-level provenance the AI has no way
/// to observe from a single `GameState` snapshot). Instead it is deliberately
/// coarse in the safe direction: ANY permanent the AI controls with a printed
/// "at the beginning of your upkeep" trigger (`TriggerMode::Phase` +
/// `phase: Some(Phase::Upkeep)`) stands the whole gate down for the AI's
/// upkeep, because CR 503.1a guarantees such a trigger is ALREADY queued (and,
/// by the time the stack is next empty, already resolved) before the
/// genuinely pristine window — so that window is provably unreachable
/// whenever one exists, and nothing is being given up by excluding it.
#[test]
fn an_upkeep_trigger_source_disables_the_whole_gate() {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    state.phase = Phase::Upkeep;
    state.active_player = AI;
    state.priority_player = AI;

    let pinger = creature(
        &mut state,
        &mut ids,
        AI,
        "Prodigal Sorcerer",
        1,
        1,
        Some("{T}: This creature deals 1 damage to any target."),
    );
    creature(&mut state, &mut ids, OPP, "Wall", 1, 5, None);

    // A separate permanent carrying Phyrexian Arena's real, verified upkeep
    // trigger. Card type is irrelevant to the predicate under test (it reads
    // `trigger_definitions`, not `core_types`), so the shared `creature()`
    // builder is reused rather than adding a second, enchantment-flavored one.
    let arena_trigger = parse_oracle_text(
        "At the beginning of your upkeep, you draw a card and you lose 1 life.",
        "Phyrexian Arena",
        &[],
        &["Enchantment".to_string()],
        &[],
    )
    .triggers
    .into_iter()
    .next()
    .expect("Phyrexian Arena parses one triggered ability");
    let arena = creature(&mut state, &mut ids, AI, "Phyrexian Arena", 0, 0, None);
    state
        .objects
        .get_mut(&arena)
        .unwrap()
        .trigger_definitions
        .push(arena_trigger);

    state.players[AI.0 as usize].life = 20;
    state.players[OPP.0 as usize].life = 20;
    state.waiting_for = WaitingFor::Priority { player: AI };
    let board = Board { state, pinger };

    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_na",
        "an upkeep-trigger source on the AI's own board must disable the low-information \
         gate entirely: whatever resolved before this priority window (possibly a real draw) \
         already invalidates the 'wait, you haven't drawn yet' premise"
    );
    assert_eq!(delta, 0.0);
}

/// Review finding: `is_low_information_window` checked `phase`, `stack`, and
/// `WaitingFor::Priority`, but never `state.active_player == ai_player`. CR
/// 117.4 rotates priority among every living player in turn
/// (`crates/engine/src/game/priority.rs`'s `priority_pass_participants`), so
/// the AI can hold `WaitingFor::Priority { player: AI }` with an empty stack
/// during the OPPONENT's upkeep too — a perfectly normal instant-speed
/// response window. The "wait, you haven't drawn yet" premise only describes
/// the AI's OWN draw step; during the opponent's upkeep the AI's next draw
/// is no closer for having waited, and deferring here trades away a live
/// interaction window — a chance to act before the opponent's draw and
/// combat — for zero informational gain. Unguarded, the gate penalised this
/// exactly as if it were the AI's own upkeep.
#[test]
fn a_deferrable_ping_is_not_held_during_the_opponents_upkeep() {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    state.phase = Phase::Upkeep;
    // The OPPONENT is active — it is their upkeep — but the AI is the one
    // currently holding priority (CR 117.4's normal rotation).
    state.active_player = OPP;
    state.priority_player = AI;

    let pinger = creature(
        &mut state,
        &mut ids,
        AI,
        "Prodigal Sorcerer",
        1,
        1,
        Some("{T}: This creature deals 1 damage to any target."),
    );
    creature(&mut state, &mut ids, OPP, "Wall", 1, 5, None);

    state.players[AI.0 as usize].life = 20;
    state.players[OPP.0 as usize].life = 20;
    state.waiting_for = WaitingFor::Priority { player: AI };
    let board = Board { state, pinger };

    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_na",
        "the patience gate must be silent during an OPPONENT's upkeep — the AI's own draw \
         is not what this priority window precedes, so there is nothing to wait for and \
         deferring only gives up a live response window"
    );
    assert_eq!(delta, 0.0);
}
