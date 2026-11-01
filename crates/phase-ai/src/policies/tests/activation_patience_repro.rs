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
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ActivationRestriction, ControllerRef,
    DelayedTriggerCondition, Effect, LibraryPosition, PlayerFilter, ResolvedAbility, TargetFilter,
    TriggerConstraint, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{DelayedTrigger, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};

use crate::ability_chain::collect_chain_effects;
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

/// A permanent carrying Phyrexian Arena's real, verified upkeep trigger. Card
/// type is irrelevant to the predicate under test (it reads
/// `trigger_definitions`, not `core_types`), so the shared `creature()` builder
/// is reused rather than adding a second, enchantment-flavored one.
fn add_upkeep_trigger_source(state: &mut GameState) {
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
    let arena = creature(state, &mut Ids(9_800), AI, "Phyrexian Arena", 0, 0, None);
    state
        .objects
        .get_mut(&arena)
        .unwrap()
        .trigger_definitions
        .push(arena_trigger);
}

/// An AI permanent carrying `subtype` (named after it), for the main-phase stand-down arms.
fn add_subtyped_permanent(state: &mut GameState, subtype: &str) -> ObjectId {
    let id = creature(state, &mut Ids(9_600), AI, subtype, 0, 0, None);
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .subtypes
        .push(subtype.to_string());
    id
}

fn verdict_for(board: &Board) -> PolicyVerdict {
    verdict_for_ability(board, 0)
}

/// [`verdict_for`] for the pinger's ability at `ability_index`.
fn verdict_for_ability(board: &Board, ability_index: usize) -> PolicyVerdict {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session.features.insert(AI, Default::default());
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;

    let candidate = CandidateAction {
        action: GameAction::ActivateAbility {
            source_id: board.pinger,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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

/// The report's "going to die or leave play" exception has no reading on the
/// AI's own turn with an empty stack: nothing targets the source and no opponent
/// attacks before the main phase. Low life alone (below `any_immediate_threat`'s
/// 40%-of-starting-life floor) therefore no longer stands patience down.
#[test]
fn low_life_alone_does_not_override_patience() {
    let board = build_board(Phase::Upkeep, 5, 20);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_hold");
    assert!(delta < 0.0, "got {delta}");
}

/// A lethal intent alone does not make an activation part of the line: only
/// the activation's own damage reaching a live opponent's life does (CR 104.3b).
/// The ping here deals 1 to an opponent at 2, so it is held.
#[test]
fn a_ping_short_of_lethal_is_held() {
    let mut board = build_board(Phase::Upkeep, 20, 2);
    // A second 1-power body puts the AI's board power at the opponent's life, so the
    // strategic intent reads PushLethal; the ping alone is still 1 short.
    creature(&mut board.state, &mut Ids(9_700), AI, "Bear", 1, 1, None);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_hold");
    assert!(delta < 0.0, "got {delta}");
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

/// CR 504.1/504.2: draw-step priority comes after the draw, so waiting buys no
/// information, but with nothing an activation now could change happening as the
/// main phase begins, waiting gives nothing up: the ability is still on offer
/// there and what it would spend now stays free for sorcery-speed plays
/// (CR 307.1). The draw step is held under its own reason, distinct from the
/// upkeep's.
#[test]
fn the_draw_step_holds_for_the_main_phase() {
    let board = build_board(Phase::Draw, 20, 20);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_hold_for_main");
    assert!(delta < 0.0, "got {delta}");
}

/// The draw step's case never rested on information, so an upkeep-trigger
/// source (which disables only the upkeep window) does not stand it down.
#[test]
fn an_upkeep_trigger_source_does_not_disable_the_draw_step() {
    let mut board = build_board(Phase::Draw, 20, 20);
    add_upkeep_trigger_source(&mut board.state);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_hold_for_main");
    assert!(delta < 0.0, "got {delta}");
}

/// CR 505.4 + CR 117.3a: the main phase opens with lore counters on the AI's Sagas (and any
/// "beginning of precombat main" triggers) before it gets priority there, which an activation
/// now could still change, so the draw step is not held.
#[test]
fn a_saga_stands_the_draw_step_down() {
    let mut board = build_board(Phase::Draw, 20, 20);
    add_subtyped_permanent(&mut board.state, "Saga");
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// A pristine upkeep runs straight into the main phase when the draw step is skipped
/// (CR 614.10), so a Saga stands it down too.
#[test]
fn a_saga_stands_the_pristine_upkeep_down() {
    let mut board = build_board(Phase::Upkeep, 20, 20);
    add_subtyped_permanent(&mut board.state, "Saga");
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// An upkeep whose triggers resolved is held for the main phase only while the main
/// phase opens with nothing; a Saga stands it down.
#[test]
fn a_saga_stands_an_upkeep_with_triggers_down() {
    let mut board = build_board(Phase::Upkeep, 20, 20);
    add_upkeep_trigger_source(&mut board.state);
    add_subtyped_permanent(&mut board.state, "Saga");
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// CR 505.5: the main phase opens with a roll to visit the AI's Attractions, read
/// through the engine's own `is_attraction_permanent`.
#[test]
fn an_attraction_stands_the_draw_step_down() {
    let mut board = build_board(Phase::Draw, 20, 20);
    let attraction = add_subtyped_permanent(&mut board.state, "Attraction");
    assert!(
        engine::game::attractions::is_attraction_permanent(&board.state.objects[&attraction]),
        "premise: the engine reads this permanent as an Attraction"
    );
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// CR 505.3: the archenemy's main phase opens with a scheme set in motion.
#[test]
fn the_archenemy_is_not_held_in_its_draw_step() {
    let mut board = build_board(Phase::Draw, 20, 20);
    board.state.archenemy = Some(AI);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// CR 117.3a: a "beginning of your first main phase" trigger (Black Market, verified
/// against MTGJSON) goes on the stack before the AI gets priority there, and a
/// sacrifice or counter now could still feed it.
#[test]
fn a_precombat_main_trigger_stands_the_draw_step_down() {
    let mut board = build_board(Phase::Draw, 20, 20);
    let trigger = parse_oracle_text(
        "At the beginning of your first main phase, add {B} for each charge counter on this enchantment.",
        "Black Market",
        &[],
        &["Enchantment".to_string()],
        &[],
    )
    .triggers
    .into_iter()
    .next()
    .expect("Black Market parses its main-phase trigger");
    assert_eq!(
        trigger.phase,
        Some(Phase::PreCombatMain),
        "premise: the trigger is a beginning-of-precombat-main trigger"
    );
    let market = add_subtyped_permanent(&mut board.state, "Black Market");
    board
        .state
        .objects
        .get_mut(&market)
        .unwrap()
        .trigger_definitions
        .push(trigger);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// A delayed trigger (CR 603.7) the AI created, firing on `condition` and drawing a card.
fn delayed_trigger(condition: DelayedTriggerCondition) -> DelayedTrigger {
    let effect = parse_oracle_text("Draw a card.", "Probe", &[], &["Sorcery".to_string()], &[])
        .abilities
        .into_iter()
        .next()
        .expect("a one-line spell parses")
        .effect;
    DelayedTrigger::new(
        condition,
        Box::new(ResolvedAbility::new(*effect, vec![], ObjectId(1), AI)),
        AI,
        ObjectId(1),
        true,
    )
}

/// `AtNextPhaseForPlayer { phase, player }` with default gate and binding.
fn at_next_phase_for(phase: Phase, player: PlayerId) -> DelayedTriggerCondition {
    DelayedTriggerCondition::AtNextPhaseForPlayer {
        phase,
        player,
        gate: Default::default(),
        binding: Default::default(),
    }
}

/// CR 603.7: a delayed trigger set for the AI's next precombat main phase fires as it
/// begins (Mana Drain, "at the beginning of your next main phase"), so the draw step is
/// not held; one set for an opponent's next main phase does not stand it down.
#[test]
fn a_delayed_main_phase_trigger_stands_the_draw_step_down() {
    let delayed = |player| delayed_trigger(at_next_phase_for(Phase::PreCombatMain, player));
    let mut board = build_board(Phase::Draw, 20, 20);
    board.state.delayed_triggers.push(delayed(OPP));
    let (_, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_hold_for_main",
        "an opponent's main phase is not the AI's"
    );
    board.state.delayed_triggers.push(delayed(AI));
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// CR 500.5 + CR 106.6: floating mana empties as the step ends, so when the activation may
/// spend it (Thran Turbine's upkeep {C}{C}, which can't be spent on spells) waiting can
/// destroy it. Mana this activation isn't allowed to spend (spell-only) leaves the hold in
/// place; the hatch asks eligibility only, so whether the cost would use the mana (a {T}-only
/// ping, an {R} leg) is left to the payment pipeline and errs toward acting.
#[test]
fn floating_mana_the_activation_could_spend_stands_patience_down() {
    use engine::types::mana::{ManaRestriction, ManaType, ManaUnit};
    let mut board = build_board(Phase::Upkeep, 20, 20);
    let lifegain = creature(
        &mut board.state,
        &mut Ids(9_400),
        AI,
        "Staff",
        0,
        1,
        Some("{2}, {T}: You gain 1 life."),
    );
    let float = |restriction: ManaRestriction| {
        ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_900),
            false,
            vec![restriction],
        )
    };
    let pool = &mut board.state.players[AI.0 as usize].mana_pool;
    pool.add(float(ManaRestriction::OnlyForSpell));
    let held = Board {
        state: board.state.clone(),
        pinger: lifegain,
    };
    assert_eq!(
        score_and_reason(&verdict_for(&held)).1,
        "activation_patience_hold",
        "spell-only mana can't pay for the activation, so the hold stands"
    );
    board.state.players[AI.0 as usize]
        .mana_pool
        .add(float(ManaRestriction::OnlyForActivation));
    let stands_down = Board {
        state: board.state.clone(),
        pinger: lifegain,
    };
    let (delta, reason) = score_and_reason(&verdict_for(&stands_down));
    assert_eq!(reason, "activation_patience_floating_mana");
    assert_eq!(delta, 0.0);
    // Eligibility only: the {T}-only ping and an {R} pinger also stand down.
    let ping = Board {
        state: board.state.clone(),
        pinger: board.pinger,
    };
    assert_eq!(
        score_and_reason(&verdict_for(&ping)).1,
        "activation_patience_floating_mana"
    );
    let red_pinger = creature(
        &mut board.state,
        &mut Ids(9_300),
        AI,
        "Red Pinger",
        1,
        1,
        Some("{R}, {T}: This creature deals 1 damage to any target."),
    );
    let red = Board {
        state: board.state.clone(),
        pinger: red_pinger,
    };
    assert_eq!(
        score_and_reason(&verdict_for(&red)).1,
        "activation_patience_floating_mana"
    );
}

/// The case from review: two opponents each controlling Kopala, Warden of Waves ("Abilities
/// your opponents activate that target a Merfolk you control cost {2} more to activate.",
/// verified against MTGJSON) raise the Sorcerer's {T} ping by {2} per Merfolk target. With
/// {C}{C} floating, whatever target is chosen, the mana empties as the step ends (CR 500.5),
/// so patience stands down without the query having to price any target assignment.
#[test]
fn floating_mana_stands_patience_down_under_target_dependent_taxes() {
    use engine::types::mana::{ManaType, ManaUnit};
    let mut ids = Ids::new();
    let mut state = GameState::new(engine::types::format::FormatConfig::standard(), 3, 4242);
    state.phase = Phase::Draw;
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
    for opponent in [PlayerId(1), PlayerId(2)] {
        let kopala = creature(
            &mut state,
            &mut ids,
            opponent,
            "Kopala, Warden of Waves",
            2,
            2,
            None,
        );
        state
            .objects
            .get_mut(&kopala)
            .unwrap()
            .card_types
            .subtypes
            .push("Merfolk".to_string());
        let tax = parse_oracle_text(
            "Abilities your opponents activate that target a Merfolk you control cost {2} more to activate.",
            "Kopala, Warden of Waves",
            &[],
            &["Creature".to_string()],
            &["Merfolk".to_string()],
        )
        .statics
        .into_iter()
        .next()
        .expect("Kopala parses its cost increase");
        state
            .objects
            .get_mut(&kopala)
            .unwrap()
            .static_definitions
            .push(tax);
    }
    state.waiting_for = WaitingFor::Priority { player: AI };
    let mut board = Board { state, pinger };
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold_for_main",
        "reach: with nothing floating the taxed ping is held"
    );
    for _ in 0..2 {
        board.state.players[AI.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(9_900),
                false,
                vec![],
            ));
    }
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_floating_mana"
    );
}

/// Granite Shard ("{3}, {T} or {R}, {T}: This artifact deals 1 damage to any target.",
/// verified against MTGJSON) in the AI's own draw step with {R} floating: the {R} branch is a
/// way to pay, so the floating mana stands patience down instead of emptying first.
#[test]
fn floating_mana_payable_by_one_branch_of_an_or_cost_stands_patience_down() {
    use engine::types::ability::AbilityCost;
    use engine::types::mana::{ManaType, ManaUnit};
    let mut board = build_board(Phase::Draw, 20, 20);
    let shard = creature(
        &mut board.state,
        &mut Ids(9_100),
        AI,
        "Granite Shard",
        0,
        1,
        Some("{3}, {T} or {R}, {T}: This artifact deals 1 damage to any target."),
    );
    assert!(
        matches!(
            board.state.objects[&shard].abilities[0].cost,
            Some(AbilityCost::OneOf { .. })
        ),
        "premise: Granite Shard's cost parses as an \"or\" cost"
    );
    board.pinger = shard;
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold_for_main",
        "reach: with nothing floating the Shard is held"
    );
    board.state.players[AI.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, ObjectId(9_900), false, vec![]));
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_floating_mana"
    );
}

/// CR 601.2f + CR 602.2b: Suppression Field ("Activated abilities cost {2} more to activate
/// unless they're mana abilities.", verified against MTGJSON) turns the printed {T}-only ping
/// into {2}, {T}; with {C}{C} floating the taxed ping stands patience down.
#[test]
fn floating_mana_that_pays_a_cost_increase_stands_patience_down() {
    use engine::types::mana::{ManaType, ManaUnit};
    let mut board = build_board(Phase::Upkeep, 20, 20);
    let field = creature(
        &mut board.state,
        &mut Ids(9_000),
        OPP,
        "Suppression Field",
        0,
        1,
        None,
    );
    let tax = parse_oracle_text(
        "Activated abilities cost {2} more to activate unless they're mana abilities.",
        "Suppression Field",
        &[],
        &["Enchantment".to_string()],
        &[],
    )
    .statics
    .into_iter()
    .next()
    .expect("Suppression Field parses its cost increase");
    board
        .state
        .objects
        .get_mut(&field)
        .unwrap()
        .static_definitions
        .push(tax);
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold",
        "reach: with nothing floating the taxed ping is held"
    );
    for _ in 0..2 {
        board.state.players[AI.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(9_900),
                false,
                vec![],
            ));
    }
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_floating_mana"
    );
}

/// CR 500.5 + CR 614.17: mana kept past the step ("you don't lose this mana as steps and
/// phases end") is still there in the main phase, so it doesn't stand patience down.
#[test]
fn retained_floating_mana_leaves_the_hold() {
    use engine::types::mana::{ManaExpiry, ManaRestriction, ManaType, ManaUnit};
    let mut board = build_board(Phase::Upkeep, 20, 20);
    let staff = creature(
        &mut board.state,
        &mut Ids(9_200),
        AI,
        "Staff",
        0,
        1,
        Some("{2}, {T}: You gain 1 life."),
    );
    let mut unit = ManaUnit::new(
        ManaType::Colorless,
        ObjectId(9_900),
        false,
        vec![ManaRestriction::OnlyForActivation],
    );
    unit.expiry = Some(ManaExpiry::EndOfTurn);
    let mut burning = board.state.clone();
    board.state.players[AI.0 as usize]
        .mana_pool
        .add(unit.clone());
    board.pinger = staff;
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold"
    );
    // Mana-burn retention (`EndOfPhaseGroup`) lasts only to the beginning phase's end, so it
    // still empties before the main phase and stands patience down.
    unit.expiry = Some(ManaExpiry::EndOfPhaseGroup);
    burning.players[AI.0 as usize].mana_pool.add(unit);
    let burning = Board {
        state: burning,
        pinger: staff,
    };
    assert_eq!(
        score_and_reason(&verdict_for(&burning)).1,
        "activation_patience_floating_mana"
    );
}

/// A permanent `owner` controls carrying the first trigger parsed from `text`.
fn add_trigger_permanent(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    let trigger = parse_oracle_text(text, name, &[], &["Artifact".to_string()], &[])
        .triggers
        .into_iter()
        .next()
        .expect("the trigger text parses");
    let id = creature(state, &mut Ids(8_900), owner, name, 0, 1, None);
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .trigger_definitions
        .push(trigger);
    id
}

/// CR 117.3a + CR 603.4: an opponent's Blinkmoth Urn ("At the beginning of each player's first
/// main phase, if this artifact is untapped, that player adds {C} for each artifact they
/// control.", verified against MTGJSON) fires in the AI's main phase too, and Voltaic Key's
/// untap ("{1}, {T}: Untap target artifact.") only helps before it does. An opponent's "your"
/// trigger (Black Market) fires on its controller's turn only, so it leaves the hold in place.
#[test]
fn an_opponents_each_player_main_phase_trigger_stands_the_draw_step_down() {
    let mut board = build_board(Phase::Draw, 20, 20);
    board.pinger = creature(
        &mut board.state,
        &mut Ids(8_800),
        AI,
        "Voltaic Key",
        0,
        1,
        Some("{1}, {T}: Untap target artifact."),
    );
    add_trigger_permanent(
        &mut board.state,
        OPP,
        "Black Market",
        "At the beginning of your first main phase, add {B} for each charge counter on this enchantment.",
    );
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold_for_main",
        "an opponent's \"your\" main-phase trigger doesn't fire in the AI's main phase"
    );
    add_trigger_permanent(
        &mut board.state,
        OPP,
        "Blinkmoth Urn",
        "At the beginning of each player's first main phase, if this artifact is untapped, that player adds {C} for each artifact they control.",
    );
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// CR 503.1a: an opponent's "each player's upkeep" trigger (Sulfuric Vortex, verified against
/// MTGJSON) also resolves in the AI's upkeep before the stack next empties, so that upkeep is no
/// longer the pristine window; it is still held for the main phase.
#[test]
fn an_opponents_each_player_upkeep_trigger_ends_the_pristine_upkeep() {
    let mut board = build_board(Phase::Upkeep, 20, 20);
    add_trigger_permanent(
        &mut board.state,
        OPP,
        "Sulfuric Vortex",
        "At the beginning of each player's upkeep, this enchantment deals 2 damage to that player.",
    );
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold_for_main"
    );
}

/// Voltaic Key, verified against MTGJSON AtomicCards.
const VOLTAIC_KEY: &str = "{1}, {T}: Untap target artifact.";
/// Howling Mine, verified against MTGJSON AtomicCards.
const HOWLING_MINE: &str = "At the beginning of each player's draw step, if this artifact is untapped, that player draws an additional card.";
/// Avaricious Dragon, verified against MTGJSON AtomicCards.
const AVARICIOUS_DRAGON: &str = "Flying\nAt the beginning of your draw step, draw an additional card.\nAt the beginning of your end step, discard your hand.";

/// [`build_board`] in `phase` with Voltaic Key as `board.pinger`, as in the Blinkmoth row. The
/// Prodigal Sorcerer it replaced stays on the battlefield; its id is returned beside the board.
fn voltaic_key_board(phase: Phase) -> (Board, ObjectId) {
    let mut board = build_board(phase, 20, 20);
    let sorcerer = board.pinger;
    board.pinger = creature(
        &mut board.state,
        &mut Ids(8_800),
        AI,
        "Voltaic Key",
        0,
        1,
        Some(VOLTAIC_KEY),
    );
    (board, sorcerer)
}

/// The trigger definition `add_trigger_permanent` put on `id`, read back for a premise.
fn only_trigger(state: &GameState, id: ObjectId) -> &TriggerDefinition {
    let mut entries = state.objects[&id].trigger_definitions.iter_unchecked();
    let entry = entries
        .next()
        .expect("premise: the permanent carries a trigger");
    assert!(entries.next().is_none(), "premise: exactly one trigger");
    entry.definition()
}

/// The policy stands down: neutral, under the not-applicable reason.
fn assert_stands_down(board: &Board, label: &str) {
    let (delta, reason) = score_and_reason(&verdict_for(board));
    assert_eq!(reason, "activation_patience_na", "{label}");
    assert_eq!(delta, 0.0, "{label}");
}

/// CR 117.3a + CR 603.4: a trigger at the beginning of the AI's draw step goes on the stack
/// before the AI next has priority, and checks its intervening-if as the step begins. Howling
/// Mine ("At the beginning of each player's draw step, if this artifact is untapped, that player
/// draws an additional card.") untapped by Voltaic Key ("{1}, {T}: Untap target artifact.") in
/// the upkeep draws the extra card; untapped in the main phase it does not, so the Key's
/// activation is not held. Any reaching draw-step trigger stands the whole upkeep down, whatever
/// the activation and whatever the trigger's condition reads now: the AI's tapped Mine, the
/// Prodigal Sorcerer's ping beside it, an opponent's Mine ("each player's"), and the AI's
/// Avaricious Dragon ("At the beginning of your draw step, draw an additional card."). An
/// opponent's Dragon triggers on its controller's draw step only, so it leaves the hold. In the
/// draw step the trigger is behind its priority (CR 504.2), so the draw step still holds.
#[test]
fn a_draw_step_trigger_stands_the_upkeep_down() {
    let (board, _) = voltaic_key_board(Phase::Upkeep);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_hold",
        "reach: the Key alone is held in the upkeep"
    );
    assert!(delta < 0.0, "reach: got {delta}");

    // (1) The AI's own Howling Mine, tapped: its intervening-if is false now.
    let (mut board, sorcerer) = voltaic_key_board(Phase::Upkeep);
    let mine = add_trigger_permanent(&mut board.state, AI, "Howling Mine", HOWLING_MINE);
    let trigger = only_trigger(&board.state, mine);
    assert!(
        trigger.phase == Some(Phase::Draw) && trigger.condition.is_some(),
        "premise: Howling Mine is a beginning-of-draw-step trigger with an intervening-if, got \
         {:?} / {:?}",
        trigger.phase,
        trigger.condition
    );
    board.state.objects.get_mut(&mine).unwrap().tapped = true;
    assert_stands_down(&board, "(1) the AI's tapped Howling Mine");

    // (2) The same board, the Prodigal Sorcerer's ping: the whole upkeep stands down.
    board.pinger = sorcerer;
    assert_stands_down(&board, "(2) the ping beside the AI's Howling Mine");

    // (3) An opponent's Howling Mine: "each player's" reaches the AI's draw step too.
    let (mut board, _) = voltaic_key_board(Phase::Upkeep);
    add_trigger_permanent(&mut board.state, OPP, "Howling Mine", HOWLING_MINE);
    assert_stands_down(&board, "(3) an opponent's Howling Mine");

    // An opponent's Avaricious Dragon: "your draw step" is its controller's, not the AI's.
    let (mut board, _) = voltaic_key_board(Phase::Upkeep);
    add_trigger_permanent(
        &mut board.state,
        OPP,
        "Avaricious Dragon",
        AVARICIOUS_DRAGON,
    );
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold",
        "an opponent's \"your draw step\" trigger does not reach the AI's draw step"
    );

    // (4) The AI's own Avaricious Dragon.
    let (mut board, _) = voltaic_key_board(Phase::Upkeep);
    let dragon =
        add_trigger_permanent(&mut board.state, AI, "Avaricious Dragon", AVARICIOUS_DRAGON);
    let trigger = only_trigger(&board.state, dragon);
    assert!(
        trigger.phase == Some(Phase::Draw)
            && trigger.constraint == Some(TriggerConstraint::OnlyDuringYourTurn),
        "premise: Avaricious Dragon's first trigger is a \"your draw step\" trigger, got {:?} / \
         {:?}",
        trigger.phase,
        trigger.constraint
    );
    assert_stands_down(&board, "(4) the AI's Avaricious Dragon");

    // CR 504.2: in the draw step the Mine's trigger is behind the AI's priority.
    let (mut board, _) = voltaic_key_board(Phase::Draw);
    add_trigger_permanent(&mut board.state, AI, "Howling Mine", HOWLING_MINE);
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold_for_main",
        "the draw step is unaffected"
    );
}

/// The same stand-down from an upkeep whose triggers resolved (Phyrexian Arena's upkeep
/// trigger), which is otherwise held for the main phase: the draw step is still ahead of it
/// (CR 117.3a + CR 603.4). An upkeep trigger is not a draw-step trigger, so Arena alone
/// leaves that hold in place.
#[test]
fn a_draw_step_trigger_stands_an_upkeep_with_triggers_down() {
    let (mut board, _) = voltaic_key_board(Phase::Upkeep);
    add_upkeep_trigger_source(&mut board.state);
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold_for_main",
        "reach: with Phyrexian Arena alone the upkeep is held for the main phase"
    );
    add_trigger_permanent(&mut board.state, AI, "Howling Mine", HOWLING_MINE);
    assert_stands_down(&board, "Phyrexian Arena and the AI's Howling Mine");
}

/// CR 603.7: a delayed trigger set for the AI's next draw step fires as that step begins, so it
/// stands the upkeep down like a printed one; one set for an opponent's next draw step does not.
/// No card creates one today; this is the delayed arm of the phase-parameterized trigger scan.
#[test]
fn a_delayed_draw_step_trigger_stands_the_upkeep_down() {
    let (mut board, _) = voltaic_key_board(Phase::Upkeep);
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold",
        "reach: without a delayed trigger the Key is held"
    );
    board
        .state
        .delayed_triggers
        .push(delayed_trigger(at_next_phase_for(Phase::Draw, OPP)));
    assert_eq!(
        score_and_reason(&verdict_for(&board)).1,
        "activation_patience_hold",
        "an opponent's next draw step is not the AI's"
    );
    board
        .state
        .delayed_triggers
        .push(delayed_trigger(at_next_phase_for(Phase::Draw, AI)));
    assert_stands_down(&board, "(1) a delayed trigger at the AI's next draw step");

    let (mut board, _) = voltaic_key_board(Phase::Upkeep);
    board
        .state
        .delayed_triggers
        .push(delayed_trigger(DelayedTriggerCondition::AtNextPhase {
            phase: Phase::Draw,
        }));
    assert_stands_down(&board, "(2) a delayed trigger at the next draw step");
}

/// CR 614.4: Words of Worship ("{1}: The next time you would draw a card this turn, you gain
/// 5 life instead.", verified against MTGJSON) must be activated before the draw it replaces.
/// In the upkeep — pristine or after Phyrexian Arena's trigger — the draw step's draw is still
/// ahead, so patience stands down; in the draw step that draw has happened and it is held.
#[test]
fn a_draw_replacement_is_not_held_while_the_draw_is_ahead() {
    use engine::types::ability::Effect;
    let words = |board: &mut Board| {
        let id = creature(
            &mut board.state,
            &mut Ids(8_700),
            AI,
            "Words of Worship",
            0,
            1,
            Some("{1}: The next time you would draw a card this turn, you gain 5 life instead."),
        );
        assert!(
            matches!(
                *board.state.objects[&id].abilities[0].effect,
                Effect::CreateDrawReplacement { .. }
            ),
            "premise: Words of Worship parses as a draw replacement"
        );
        board.pinger = id;
    };
    let mut pristine = build_board(Phase::Upkeep, 20, 20);
    words(&mut pristine);
    assert_eq!(
        score_and_reason(&verdict_for(&pristine)).1,
        "activation_patience_draw_replacement"
    );
    let mut after_trigger = build_board(Phase::Upkeep, 20, 20);
    add_upkeep_trigger_source(&mut after_trigger.state);
    words(&mut after_trigger);
    let (delta, reason) = score_and_reason(&verdict_for(&after_trigger));
    assert_eq!(reason, "activation_patience_draw_replacement");
    assert_eq!(delta, 0.0);
    let mut draw_step = build_board(Phase::Draw, 20, 20);
    words(&mut draw_step);
    assert_eq!(
        score_and_reason(&verdict_for(&draw_step)).1,
        "activation_patience_hold_for_main",
        "control: once the draw is behind, the replacement can wait"
    );
}

const SENSEIS_DIVINING_TOP: &str = "{1}: Look at the top three cards of your library, then put them back in any order.\n{T}: Draw a card, then put this artifact on top of its owner's library.";
const WATCHFUL_AUTOMATON: &str =
    "{2}{U}: Scry 1. (Look at the top card of your library. You may put that card on the bottom.)";
const ELEMENTAL_AUGURY: &str =
    "{3}: Look at the top three cards of target player's library, then put them back in any order.";
const CRYSTAL_BALL: &str = "{1}, {T}: Scry 2. (Look at the top two cards of your library, then put any number of them on the bottom and the rest on top in any order.)";

/// Put `name` (built from its verbatim Oracle text) on the AI's battlefield as the board's
/// activation source.
fn set_arranger(board: &mut Board, name: &str, oracle_text: &str) -> ObjectId {
    let id = creature(
        &mut board.state,
        &mut Ids(8_800),
        AI,
        name,
        1,
        1,
        Some(oracle_text),
    );
    board.pinger = id;
    id
}

/// Whose library an arrangement's player slot names, as the rows pin it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// "your library" (`TargetFilter::Controller`).
    Controller,
    /// "target player's library" (`TargetFilter::Player`, CR 115.1): the AI may name itself.
    AnyPlayer,
    /// "target opponent's library": the AI can't name itself.
    Opponent,
    /// "its controller" read off a parent target, which the player-filter authority can't answer
    /// without ability context.
    ParentTargetController,
}

/// The player slot of an arrangement shape the hatch reads (CR 701.22a scry, CR 701.25a
/// surveil, CR 701.20e a look-and-reorder dig), or `None` when `effect` is not one.
fn arrangement_slot(effect: &engine::types::ability::Effect) -> Option<Slot> {
    use engine::types::ability::{ControllerRef, Effect, TargetFilter};
    let player = match effect {
        Effect::Scry { target: player, .. }
        | Effect::Surveil { target: player, .. }
        | Effect::Dig {
            player,
            destination: Some(Zone::Library),
            rest_destination: Some(Zone::Library),
            keep_count: None,
            keep_count_expr: None,
            ..
        } => player,
        _ => return None,
    };
    Some(match player {
        TargetFilter::Controller => Slot::Controller,
        TargetFilter::Player => Slot::AnyPlayer,
        TargetFilter::ParentTargetController => Slot::ParentTargetController,
        TargetFilter::Typed(typed)
            if typed.type_filters.is_empty()
                && typed.properties.is_empty()
                && typed.controller == Some(ControllerRef::Opponent) =>
        {
            Slot::Opponent
        }
        other => panic!("no row pins an arrangement slot of {other:?}"),
    })
}

/// Rewrite Elemental Augury's `Dig` player slot to `player`, keeping the rest of its arrangement.
fn set_dig_player(
    ability: &mut engine::types::ability::AbilityDefinition,
    player: engine::types::ability::TargetFilter,
) {
    match &mut *ability.effect {
        engine::types::ability::Effect::Dig { player: slot, .. } => *slot = player,
        other => panic!("premise: Elemental Augury parses as a Dig, got {other:?}"),
    }
}

/// CR 504.1 + CR 121.1: the draw step's draw takes the top card, and in the upkeep it is still
/// ahead. An arrangement of the top of the AI's own library made in the upkeep chooses the card
/// that draw takes; one made later arranges a later draw, so waiting forfeits that choice and
/// the arrangement is not held. That holds whatever the ability costs: one paid in mana, life
/// or energy, one that taps its source, and one capped by a restriction (CR 602.5b, CR 702.177a
/// exhaust) are all exempt, and whether the tap or the capped activation is worth spending is
/// left to resource valuation. In the draw step the draw is behind and every arrangement is
/// held; an ability that only draws is held in the upkeep
/// (`an_upkeep_own_library_change_is_not_held_while_the_draw_is_ahead`).
///
/// The arrangement need not name the AI's library outright. Elemental Augury's "target player's
/// library" (CR 115.1) can be the AI's own, so it is exempt; so is a slot the player-filter
/// authority can't answer without ability context, which may still name the AI when the ability
/// resolves. Only a slot that can't name the AI, "target opponent's library", is held.
#[test]
fn an_upkeep_library_top_arrangement_is_not_held_while_the_draw_is_ahead() {
    use engine::types::ability::{AbilityCost, CostCategory, QuantityExpr};

    struct Row {
        label: &'static str,
        name: &'static str,
        oracle_text: &'static str,
        ability_index: usize,
        after_trigger: bool,
        phase: Phase,
        edit: Option<fn(&mut engine::types::ability::AbilityDefinition)>,
        categories: &'static [CostCategory],
        slot: Option<Slot>,
        expected: &'static str,
    }
    let row = |label, name, oracle_text, expected| Row {
        label,
        name,
        oracle_text,
        ability_index: 0,
        after_trigger: false,
        phase: Phase::Upkeep,
        edit: None,
        categories: &[CostCategory::ManaOnly],
        slot: Some(Slot::Controller),
        expected,
    };
    let rows = [
        // Positives: arrangements in the upkeep, whatever their cost.
        row(
            "Top ability 0 (Dig reorder), pristine upkeep",
            "Sensei's Divining Top",
            SENSEIS_DIVINING_TOP,
            "activation_patience_library_top",
        ),
        row(
            "Watchful Automaton (Scry), pristine upkeep",
            "Watchful Automaton",
            WATCHFUL_AUTOMATON,
            "activation_patience_library_top",
        ),
        Row {
            after_trigger: true,
            ..row(
                "Rummaging Wizard (Surveil), upkeep after a trigger",
                "Rummaging Wizard",
                "{2}{U}: Surveil 1. (Look at the top card of your library. You may put that card into your graveyard.)",
                "activation_patience_library_top",
            )
        },
        Row {
            // The fixture parses without MTGJSON's keyword list, so its "Flying, trample" line
            // takes slot 0 and the printed Surveil ability is at index 1 here.
            ability_index: 1,
            categories: &[CostCategory::PaysLife],
            ..row(
                "Doom Whisperer (PaysLife), pristine upkeep",
                "Doom Whisperer",
                "Flying, trample\nPay 2 life: Surveil 2. (Look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)",
                "activation_patience_library_top",
            )
        },
        Row {
            edit: Some(|ability| {
                ability.cost = Some(AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                });
            }),
            categories: &[CostCategory::PaysEnergy],
            ..row(
                "Watchful Automaton paying energy, pristine upkeep",
                "Watchful Automaton",
                WATCHFUL_AUTOMATON,
                "activation_patience_library_top",
            )
        },
        // Positives that spend a tap or a capped activation: still exempt in the upkeep.
        Row {
            categories: &[CostCategory::ManaOnly, CostCategory::TapsSelf],
            ..row(
                "Crystal Ball (taps its source), pristine upkeep",
                "Crystal Ball",
                CRYSTAL_BALL,
                "activation_patience_library_top",
            )
        },
        Row {
            // As for Doom Whisperer, the unlisted "Defender" keyword line takes slot 0.
            ability_index: 1,
            after_trigger: true,
            categories: &[CostCategory::ManaOnly, CostCategory::TapsSelf],
            ..row(
                "Coastal Bulwark (taps its source), upkeep after a trigger",
                "Coastal Bulwark",
                "Defender\nThis creature gets +2/+0 as long as you control an Island.\n{2}, {T}: Surveil 1. (Look at the top card of your library. You may put that card into your graveyard.)",
                "activation_patience_library_top",
            )
        },
        Row {
            ability_index: 1,
            ..row(
                "Audacious Knuckleblade ability 1 (exhaust), pristine upkeep",
                "Audacious Knuckleblade",
                "Exhaust — {2}{G}: Seek a card named Audacious Knuckleblade and put it onto the battlefield tapped.\nExhaust — {1}{U}: Surveil 2, then untap this creature.\nExhaust — {R}: Creatures you control gain haste until end of turn.",
                "activation_patience_library_top",
            )
        },
        Row {
            edit: Some(|ability| {
                ability
                    .activation_restrictions
                    .push(ActivationRestriction::OnlyOnceEachTurn);
            }),
            ..row(
                "Watchful Automaton, once each turn, pristine upkeep",
                "Watchful Automaton",
                WATCHFUL_AUTOMATON,
                "activation_patience_library_top",
            )
        },
        Row {
            edit: Some(|ability| {
                ability
                    .activation_restrictions
                    .push(ActivationRestriction::MaxTimesEachTurn { count: 2 });
            }),
            ..row(
                "Watchful Automaton, at most twice each turn, pristine upkeep",
                "Watchful Automaton",
                WATCHFUL_AUTOMATON,
                "activation_patience_library_top",
            )
        },
        // Siblings and boundaries.
        Row {
            phase: Phase::Draw,
            ..row(
                "Top ability 0 in the draw step (the draw is behind)",
                "Sensei's Divining Top",
                SENSEIS_DIVINING_TOP,
                "activation_patience_hold_for_main",
            )
        },
        Row {
            phase: Phase::Draw,
            categories: &[CostCategory::ManaOnly, CostCategory::TapsSelf],
            ..row(
                "Crystal Ball in the draw step (the draw is behind)",
                "Crystal Ball",
                CRYSTAL_BALL,
                "activation_patience_hold_for_main",
            )
        },
        Row {
            slot: Some(Slot::AnyPlayer),
            ..row(
                "Elemental Augury (target player's library), pristine upkeep",
                "Elemental Augury",
                ELEMENTAL_AUGURY,
                "activation_patience_library_top",
            )
        },
        Row {
            edit: Some(|ability| {
                set_dig_player(ability, engine::types::ability::TargetFilter::ParentTargetController);
            }),
            slot: Some(Slot::ParentTargetController),
            ..row(
                "Elemental Augury with a slot the authority can't answer, pristine upkeep",
                "Elemental Augury",
                ELEMENTAL_AUGURY,
                "activation_patience_library_top",
            )
        },
        Row {
            phase: Phase::Draw,
            slot: Some(Slot::AnyPlayer),
            ..row(
                "Elemental Augury in the draw step (the draw is behind)",
                "Elemental Augury",
                ELEMENTAL_AUGURY,
                "activation_patience_hold_for_main",
            )
        },
        Row {
            ability_index: 1,
            categories: &[CostCategory::TapsSelf],
            slot: None,
            ..row(
                "Top ability 1 (its draw puts the Top back on top), pristine upkeep",
                "Sensei's Divining Top",
                SENSEIS_DIVINING_TOP,
                "activation_patience_library_top",
            )
        },
        Row {
            // Second Sight's and Tahngarth's Glare's "target opponent's library" slot, on an
            // activated ability: the AI can't name itself, so the arrangement can't reach its draw.
            edit: Some(|ability| {
                set_dig_player(
                    ability,
                    engine::types::ability::TargetFilter::Typed(
                        engine::types::ability::TypedFilter::default()
                            .controller(engine::types::ability::ControllerRef::Opponent),
                    ),
                );
            }),
            slot: Some(Slot::Opponent),
            ..row(
                "Elemental Augury on target opponent's library, pristine upkeep",
                "Elemental Augury",
                ELEMENTAL_AUGURY,
                "activation_patience_hold",
            )
        },
    ];

    for row in rows {
        let mut board = build_board(row.phase, 20, 20);
        if row.after_trigger {
            add_upkeep_trigger_source(&mut board.state);
        }
        let id = set_arranger(&mut board, row.name, row.oracle_text);
        if let Some(edit) = row.edit {
            edit(
                &mut Arc::make_mut(&mut board.state.objects.get_mut(&id).unwrap().abilities)
                    [row.ability_index],
            );
        }
        let ability = &board.state.objects[&id].abilities[row.ability_index];
        assert_eq!(
            arrangement_slot(&ability.effect),
            row.slot,
            "{}: premise: the effect's arrangement slot, got {:?}",
            row.label,
            ability.effect
        );
        assert_eq!(
            ability.cost_categories(),
            row.categories,
            "{}: premise: cost categories",
            row.label
        );
        if row.name == "Audacious Knuckleblade" {
            assert!(
                ability
                    .activation_restrictions
                    .contains(&ActivationRestriction::OnlyOnce),
                "{}: premise: exhaust parses as an only-once restriction (CR 702.177a)",
                row.label
            );
        }
        let (delta, reason) = score_and_reason(&verdict_for_ability(&board, row.ability_index));
        assert_eq!(reason, row.expected, "{}", row.label);
        if row.expected == "activation_patience_library_top" {
            assert_eq!(delta, 0.0, "{}", row.label);
        } else {
            assert!(delta < 0.0, "{}: held, got {delta}", row.label);
        }
    }
}

/// The player slot "target opponent" (or "target opponent's library"), which can't name the AI.
fn names_an_opponent(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(typed)
            if typed.type_filters.is_empty()
                && typed.controller == Some(ControllerRef::Opponent)
    )
}

/// `{1}, {T}`-style look at the top of a library that keeps nothing itself (CR 701.20e), with the
/// player whose library it is.
fn look_player(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Dig {
            player,
            destination: None,
            keep_count: Some(0),
            rest_destination: None,
            ..
        } => Some(player),
        _ => None,
    }
}

const ACADEMY_RUINS: &str = "{T}: Add {C}.\n{1}{U}, {T}: Put target artifact card from your graveyard on top of your library.";
const EXCAVATED_WALL: &str =
    "Defender\n{1}, {T}: Mill a card. (Put the top card of your library into your graveyard.)";

/// CR 504.1 + CR 121.1: in the upkeep the draw step's draw is still ahead, and any change to a
/// library that can be the AI's own changes what that draw takes, so the activation does not
/// commute with it and waiting does not reproduce it. Each row is a shape of that class or a
/// shape that stays held, built from its verbatim Oracle text; each asserts its parse premise,
/// and each hold row's reason is its positive reach.
///
/// Held: a draw (both draws take the same two cards), a search with its own instructions, a
/// look whose card nothing then acts on, and a change that can only reach an opponent's library
/// (a scope or slot that can't name the AI). Held in the draw step whatever the shape: CR 504.2
/// puts the draw behind that priority, so the main phase reproduces the activation.
///
/// Reito Lantern pins an accepted over-reach: bottom placement changes this draw only on an
/// empty library (CR 704.5b), and a card slot names no player, so it fails closed.
#[test]
fn an_upkeep_own_library_change_is_not_held_while_the_draw_is_ahead() {
    struct Row {
        label: &'static str,
        name: &'static str,
        oracle_text: &'static str,
        ability_index: usize,
        premise: fn(&AbilityDefinition) -> bool,
        phase: Phase,
        after_trigger: bool,
        expected: &'static str,
    }
    const TOP: &str = "activation_patience_library_top";
    const HOLD: &str = "activation_patience_hold";
    const HOLD_FOR_MAIN: &str = "activation_patience_hold_for_main";
    let row = |label,
               name,
               oracle_text,
               ability_index,
               premise: fn(&AbilityDefinition) -> bool,
               expected| Row {
        label,
        name,
        oracle_text,
        ability_index,
        premise,
        phase: Phase::Upkeep,
        after_trigger: false,
        expected,
    };
    let puts_on_top = |ability: &AbilityDefinition| {
        matches!(
            *ability.effect,
            Effect::PutAtLibraryPosition {
                position: LibraryPosition::Top,
                ..
            }
        ) && ability.condition.is_none()
    };
    let mills_controller = |ability: &AbilityDefinition| {
        matches!(
            *ability.effect,
            Effect::Mill {
                target: TargetFilter::Controller,
                ..
            }
        ) && ability.player_scope.is_none()
    };
    let reveal_gates_its_payoff = |ability: &AbilityDefinition| {
        matches!(
            *ability.effect,
            Effect::RevealTop {
                player: TargetFilter::Controller,
                ..
            }
        ) && ability.condition.is_none()
            && ability.sub_ability.as_ref().is_some_and(|sub| {
                matches!(
                    sub.condition,
                    Some(AbilityCondition::RevealedHasCardType { .. })
                )
            })
    };
    let searches = |ability: &AbilityDefinition| {
        matches!(
            collect_chain_effects(ability).as_slice(),
            [
                Effect::SearchLibrary { .. },
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    ..
                },
                Effect::Shuffle { .. }
            ]
        )
    };
    let rows = [
        // Top placement.
        row(
            "Academy Ruins ability 1 (a card put on top)",
            "Academy Ruins",
            ACADEMY_RUINS,
            1,
            puts_on_top,
            TOP,
        ),
        row(
            "Volrath's Stronghold ability 1 (a card put on top)",
            "Volrath's Stronghold",
            "{T}: Add {C}.\n{1}{B}, {T}: Put target creature card from your graveyard on top of your library.",
            1,
            puts_on_top,
            TOP,
        ),
        // Mill: whose library reads `player_scope` first, then the slot.
        row(
            "Excavated Wall (self-mill)",
            "Excavated Wall",
            EXCAVATED_WALL,
            1,
            mills_controller,
            TOP,
        ),
        row(
            "Cathartic Adept (target player mills)",
            "Cathartic Adept",
            "{T}: Target player mills a card.",
            0,
            |ability| {
                matches!(
                    *ability.effect,
                    Effect::Mill {
                        target: TargetFilter::Player,
                        ..
                    }
                )
            },
            TOP,
        ),
        row(
            "Whetstone (each player mills)",
            "Whetstone",
            "{3}: Each player mills two cards.",
            0,
            |ability| {
                matches!(*ability.effect, Effect::Mill { .. })
                    && ability.player_scope == Some(PlayerFilter::All)
            },
            TOP,
        ),
        row(
            "Folio of Fancies ability 1 (each opponent mills)",
            "Folio of Fancies",
            "Players have no maximum hand size.\n{X}{X}, {T}: Each player draws X cards.\n{2}{U}, {T}: Each opponent mills cards equal to the number of cards in their hand.",
            1,
            |ability| {
                matches!(
                    *ability.effect,
                    Effect::Mill {
                        target: TargetFilter::Controller,
                        ..
                    }
                ) && ability.player_scope == Some(PlayerFilter::Opponent)
            },
            HOLD,
        ),
        // Cards taken from the top.
        row(
            "Ornate Kanzashi (target opponent exiles their top card)",
            "Ornate Kanzashi",
            "{2}, {T}: Target opponent exiles the top card of their library. You may play that card this turn.",
            0,
            |ability| {
                matches!(
                    &*ability.effect,
                    Effect::ExileTop { player, .. } if names_an_opponent(player)
                )
            },
            HOLD,
        ),
        row(
            "Thoughtpicker Witch (a look at an opponent's library whose card moves)",
            "Thoughtpicker Witch",
            "{1}, Sacrifice a creature: Look at the top two cards of target opponent's library, then exile one of them.",
            0,
            |ability| {
                look_player(&ability.effect).is_some_and(names_an_opponent)
                    && ability.sub_ability.as_ref().is_some_and(|sub| {
                        matches!(
                            *sub.effect,
                            Effect::ChangeZone {
                                origin: None,
                                target: TargetFilter::ParentTarget,
                                ..
                            }
                        )
                    })
            },
            HOLD,
        ),
        row(
            "Thought Dissector (a reveal of an opponent's library whose card moves)",
            "Thought Dissector",
            "{X}, {T}: Target opponent reveals cards from the top of their library until an artifact card or X cards are revealed, whichever comes first. If an artifact card is revealed this way, put it onto the battlefield under your control and sacrifice this artifact. Put the rest of the revealed cards into that player's graveyard.",
            0,
            |ability| {
                let chain = collect_chain_effects(ability);
                matches!(
                    chain.first(),
                    Some(Effect::RevealTop { player, .. }) if names_an_opponent(player)
                ) && chain.iter().any(|effect| {
                    matches!(
                        effect,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Battlefield,
                            target: TargetFilter::ParentTarget,
                            ..
                        }
                    )
                }) && chain.iter().any(|effect| {
                    matches!(
                        effect,
                        Effect::ChangeZoneAll {
                            origin: Some(Zone::Exile),
                            destination: Zone::Graveyard,
                            ..
                        }
                    )
                })
            },
            HOLD,
        ),
        row(
            "Dark-Dweller Oracle (exiles the AI's top card)",
            "Dark-Dweller Oracle",
            "{1}, Sacrifice a creature: Exile the top card of your library. You may play that card this turn. (You still pay its costs. You can play a land this way only if you have an available land play remaining.)",
            0,
            |ability| {
                matches!(
                    *ability.effect,
                    Effect::ExileTop {
                        player: TargetFilter::Controller,
                        ..
                    }
                )
            },
            TOP,
        ),
        row(
            "Legion Vanguard (explore, a node with no slot)",
            "Legion Vanguard",
            "{1}, Sacrifice another creature: This creature explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on this creature, then put the card back or put it into your graveyard.)",
            0,
            |ability| matches!(*ability.effect, Effect::Explore),
            TOP,
        ),
        row(
            "Fomori Vault ability 1 (a dig that keeps one)",
            "Fomori Vault",
            "{T}: Add {C}.\n{3}, {T}, Discard a card: Look at the top X cards of your library, where X is the number of artifacts you control. Put one of those cards into your hand and the rest on the bottom of your library in a random order.",
            1,
            |ability| {
                matches!(
                    *ability.effect,
                    Effect::Dig {
                        keep_count: Some(1),
                        destination: Some(Zone::Hand),
                        ..
                    }
                )
            },
            TOP,
        ),
        row(
            "Feldon's Cane (the graveyard shuffled into the library)",
            "Feldon's Cane",
            "{T}, Exile this artifact: Shuffle your graveyard into your library.",
            0,
            |ability| {
                matches!(
                    collect_chain_effects(ability).as_slice(),
                    [
                        Effect::ChangeZoneAll {
                            destination: Zone::Library,
                            ..
                        },
                        Effect::Shuffle { .. }
                    ]
                )
            },
            TOP,
        ),
        // A look or reveal whose card then moves: the look answers for the mover's anaphor.
        row(
            "Zoologist (a reveal whose card then moves)",
            "Zoologist",
            "{3}{G}, {T}: Reveal the top card of your library. If it's a creature card, put it onto the battlefield. Otherwise, put it into your graveyard.",
            0,
            |ability| {
                matches!(
                    collect_chain_effects(ability).as_slice(),
                    [
                        Effect::RevealTop {
                            player: TargetFilter::Controller,
                            ..
                        },
                        Effect::ChangeZone { origin: None, .. },
                        ..
                    ]
                )
            },
            TOP,
        ),
        row(
            "Lurking Informant (a look whose card then moves)",
            "Lurking Informant",
            "({U/B} can be paid with either {U} or {B}.)\n{2}, {T}: Look at the top card of target player's library. You may put that card into that player's graveyard.",
            0,
            |ability| {
                look_player(&ability.effect) == Some(&TargetFilter::Player)
                    && ability.sub_ability.as_ref().is_some_and(|sub| {
                        matches!(*sub.effect, Effect::ChangeZone { origin: None, .. })
                    })
            },
            TOP,
        ),
        // A reveal whose card then gates a payoff: nothing moves, but which card it reads does.
        row(
            "Iron Lad, Diverging Destiny ability 1 (a reveal whose card gates a draw)",
            "Iron Lad, Diverging Destiny",
            "Flying, vigilance\nYou may look at the top card of your library any time.\n{T}: Reveal the top card of your library. If it's an artifact card, draw a card.",
            1,
            reveal_gates_its_payoff,
            TOP,
        ),
        row(
            "Brutal Deceiver ability 1 (a reveal whose card gates a pump)",
            "Brutal Deceiver",
            "{1}: Look at the top card of your library.\n{2}: Reveal the top card of your library. If it's a land card, this creature gets +1/+0 and gains first strike until end of turn. Activate only once each turn.",
            1,
            reveal_gates_its_payoff,
            TOP,
        ),
        // A shuffle with no search, and bottom placement.
        row(
            "Myr Mindservant (a bare shuffle)",
            "Myr Mindservant",
            "{2}, {T}: Shuffle your library.",
            0,
            |ability| {
                matches!(
                    collect_chain_effects(ability).as_slice(),
                    [Effect::Shuffle { .. }]
                )
            },
            TOP,
        ),
        row(
            "Reito Lantern (bottom placement, the accepted over-reach)",
            "Reito Lantern",
            "{3}: Put target card from a graveyard on the bottom of its owner's library.",
            0,
            |ability| {
                matches!(
                    *ability.effect,
                    Effect::PutAtLibraryPosition {
                        position: LibraryPosition::Bottom,
                        ..
                    }
                )
            },
            TOP,
        ),
        // Costs (CR 602.1a): the effect after the colon leaves the library alone.
        row(
            "Hidden Retreat (a card put on top as a cost)",
            "Hidden Retreat",
            "Put a card from your hand on top of your library: Prevent all damage that would be dealt by target instant or sorcery spell this turn.",
            0,
            |ability| {
                matches!(ability.cost, Some(AbilityCost::EffectCost { .. }))
                    && !ability.effect.moves_card_to_or_from_library()
            },
            TOP,
        ),
        row(
            "Leashling (a card put on top as a cost)",
            "Leashling",
            "Put a card from your hand on top of your library: Return this creature to its owner's hand.",
            0,
            |ability| {
                matches!(ability.cost, Some(AbilityCost::EffectCost { .. }))
                    && matches!(
                        *ability.effect,
                        Effect::Bounce {
                            target: TargetFilter::SelfRef,
                            ..
                        }
                    )
            },
            TOP,
        ),
        row(
            "Royal Herbalist (the top card exiled as a cost, inside a composite)",
            "Royal Herbalist",
            "{2}, Exile the top card of your library: You gain 1 life.",
            0,
            |ability| {
                matches!(
                    &ability.cost,
                    Some(AbilityCost::Composite { costs }) if costs.iter().any(|cost| matches!(
                        cost,
                        AbilityCost::Exile {
                            zone: Some(Zone::Library),
                            ..
                        }
                    ))
                ) && !ability.effect.moves_card_to_or_from_library()
            },
            TOP,
        ),
        // Modes: the library change is only in one.
        row(
            "Bow of Nylea (a library change in one mode only)",
            "Bow of Nylea",
            "Attacking creatures you control have deathtouch.\n{1}{G}, {T}: Choose one —\n• Put a +1/+1 counter on target creature.\n• Bow of Nylea deals 2 damage to target creature with flying.\n• You gain 3 life.\n• Put up to four target cards from your graveyard on the bottom of your library in any order.",
            0,
            |ability| {
                !collect_chain_effects(ability)
                    .iter()
                    .any(|effect| effect.moves_card_to_or_from_library())
                    && ability.mode_abilities.iter().any(|mode| {
                        matches!(
                            *mode.effect,
                            Effect::PutAtLibraryPosition {
                                position: LibraryPosition::Bottom,
                                ..
                            }
                        )
                    })
            },
            TOP,
        ),
        // A tutored card put back on the library.
        row(
            "Sterling Grove (a tutored card put on top)",
            "Sterling Grove",
            "Other enchantments you control have shroud. (They can't be the targets of spells or abilities.)\n{1}, Sacrifice this enchantment: Search your library for an enchantment card, reveal it, then shuffle and put that card on top.",
            0,
            |ability| {
                matches!(
                    collect_chain_effects(ability).as_slice(),
                    [
                        Effect::SearchLibrary { .. },
                        Effect::Shuffle { .. },
                        Effect::PutAtLibraryPosition {
                            target: TargetFilter::Any,
                            position: LibraryPosition::Top,
                            ..
                        },
                        ..
                    ]
                )
            },
            TOP,
        ),
        // Held: draws commute with the draw (CR 121.1).
        row(
            "Jayemdae Tome (a draw)",
            "Jayemdae Tome",
            "{4}, {T}: Draw a card.",
            0,
            |ability| {
                matches!(
                    collect_chain_effects(ability).as_slice(),
                    [Effect::Draw { .. }]
                )
            },
            HOLD,
        ),
        row(
            "Merfolk Looter (a draw, then a discard)",
            "Merfolk Looter",
            "{T}: Draw a card, then discard a card.",
            0,
            |ability| {
                matches!(
                    collect_chain_effects(ability).as_slice(),
                    [Effect::Draw { .. }, Effect::Discard { .. }]
                )
            },
            HOLD,
        ),
        // Held: a search and its own instructions.
        row(
            "Flooded Strand (a search and its shuffle)",
            "Flooded Strand",
            "{T}, Pay 1 life, Sacrifice this land: Search your library for a Plains or Island card, put it onto the battlefield, then shuffle.",
            0,
            searches,
            HOLD,
        ),
        row(
            "Expedition Map (a search and its shuffle)",
            "Expedition Map",
            "{2}, {T}, Sacrifice this artifact: Search your library for a land card, reveal it, put it into your hand, then shuffle.",
            0,
            searches,
            HOLD,
        ),
        // Held: a look whose card nothing then acts on.
        row(
            "Orcish Spy (a look alone)",
            "Orcish Spy",
            "{T}: Look at the top three cards of target player's library.",
            0,
            |ability| {
                look_player(&ability.effect) == Some(&TargetFilter::Player)
                    && ability.sub_ability.is_none()
            },
            HOLD,
        ),
        row(
            "Mishra's Bauble (a look and a delayed trigger)",
            "Mishra's Bauble",
            "{T}, Sacrifice this artifact: Look at the top card of target player's library. Draw a card at the beginning of the next turn's upkeep.",
            0,
            |ability| {
                look_player(&ability.effect).is_some()
                    && ability.sub_ability.as_ref().is_some_and(|sub| {
                        matches!(*sub.effect, Effect::CreateDelayedTrigger { .. })
                    })
            },
            HOLD,
        ),
        // The draw step: the draw is behind (CR 504.2).
        Row {
            phase: Phase::Draw,
            ..row(
                "Academy Ruins ability 1 in the draw step",
                "Academy Ruins",
                ACADEMY_RUINS,
                1,
                puts_on_top,
                HOLD_FOR_MAIN,
            )
        },
        Row {
            phase: Phase::Draw,
            ..row(
                "Excavated Wall in the draw step",
                "Excavated Wall",
                EXCAVATED_WALL,
                1,
                mills_controller,
                HOLD_FOR_MAIN,
            )
        },
        // An upkeep whose triggers have resolved still precedes the draw.
        Row {
            after_trigger: true,
            ..row(
                "Academy Ruins ability 1, upkeep after a trigger",
                "Academy Ruins",
                ACADEMY_RUINS,
                1,
                puts_on_top,
                TOP,
            )
        },
    ];

    for row in rows {
        let mut board = build_board(row.phase, 20, 20);
        if row.after_trigger {
            add_upkeep_trigger_source(&mut board.state);
        }
        let id = set_arranger(&mut board, row.name, row.oracle_text);
        let ability = &board.state.objects[&id].abilities[row.ability_index];
        assert!(
            (row.premise)(ability),
            "{}: premise: the parse shape, got {ability:?}",
            row.label
        );
        let (delta, reason) = score_and_reason(&verdict_for_ability(&board, row.ability_index));
        assert_eq!(reason, row.expected, "{}", row.label);
        if row.expected == TOP {
            assert_eq!(delta, 0.0, "{}", row.label);
        } else {
            assert!(delta < 0.0, "{}: held, got {delta}", row.label);
        }
    }
}

/// CR 602.5: an ability restricted to the upkeep can't be activated in the main
/// phase, so it is not deferrable and patience stays silent.
#[test]
fn an_upkeep_only_ability_is_not_deferrable() {
    let mut board = build_board(Phase::Upkeep, 20, 20);
    Arc::make_mut(
        &mut board
            .state
            .objects
            .get_mut(&board.pinger)
            .unwrap()
            .abilities,
    )[0]
    .activation_restrictions
    .push(ActivationRestriction::DuringYourUpkeep);
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// An opponent's draw step is a live response window for the AI, not a wait.
#[test]
fn a_deferrable_ping_is_not_held_during_the_opponents_draw_step() {
    let mut board = build_board(Phase::Draw, 20, 20);
    board.state.active_player = OPP;
    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(reason, "activation_patience_na");
    assert_eq!(delta, 0.0);
}

/// Review finding (PR #8696): the conditional escape hatch had no test, unlike its two
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

/// Review finding (PR #8696): `is_low_information_window` (now `patient_window`) treated EVERY
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
/// `phase: Some(Phase::Upkeep)`) stands the upkeep window down for the AI's
/// upkeep, because CR 503.1a guarantees such a trigger is ALREADY queued (and,
/// by the time the stack is next empty, already resolved) before the
/// genuinely pristine window — so that window is provably unreachable
/// whenever one exists, and nothing is being given up by excluding it.
#[test]
fn an_upkeep_trigger_source_disables_the_upkeep_window_but_still_holds_for_main() {
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

    add_upkeep_trigger_source(&mut state);

    state.players[AI.0 as usize].life = 20;
    state.players[OPP.0 as usize].life = 20;
    state.waiting_for = WaitingFor::Priority { player: AI };
    let board = Board { state, pinger };

    let (delta, reason) = score_and_reason(&verdict_for(&board));
    assert_eq!(
        reason, "activation_patience_hold_for_main",
        "an upkeep-trigger source on the AI's own board must disable the low-information \
         reason: whatever resolved before this priority window (possibly a real draw) \
         already invalidates the 'wait, you haven't drawn yet' premise. The main phase \
         still offers everything this window does, so the activation is held for it"
    );
    assert!(delta < 0.0, "got {delta}");
}

/// Review finding: `is_low_information_window` (now `patient_window`) checked `phase`, `stack`, and
/// `WaitingFor::Priority`, but never `state.active_player == ai_player`. CR
/// 117.3d passes priority to the next player in turn order
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
    // currently holding priority (CR 117.3d's normal rotation).
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
