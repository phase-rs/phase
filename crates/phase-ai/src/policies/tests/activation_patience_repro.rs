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
use engine::types::ability::ActivationRestriction;
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

/// CR 603.7: a delayed trigger set for the AI's next precombat main phase fires as it
/// begins (Mana Drain, "at the beginning of your next main phase"), so the draw step is
/// not held; one set for an opponent's next main phase does not stand it down.
#[test]
fn a_delayed_main_phase_trigger_stands_the_draw_step_down() {
    use engine::types::ability::{DelayedTriggerCondition, ResolvedAbility};
    use engine::types::game_state::DelayedTrigger;
    let delayed = |player| {
        let effect = parse_oracle_text("Draw a card.", "Probe", &[], &["Sorcery".to_string()], &[])
            .abilities
            .into_iter()
            .next()
            .expect("a one-line spell parses")
            .effect;
        DelayedTrigger::new(
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player,
                gate: Default::default(),
                binding: Default::default(),
            },
            Box::new(ResolvedAbility::new(*effect, vec![], ObjectId(1), AI)),
            AI,
            ObjectId(1),
            true,
        )
    };
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
fn add_trigger_permanent(state: &mut GameState, owner: PlayerId, name: &str, text: &str) {
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
