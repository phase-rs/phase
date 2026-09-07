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
//! One negative arm (hold at upkeep) and four positive controls, one per escape
//! hatch plus the phase gate. A policy that simply always penalised would pass
//! the negative arm alone.

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
