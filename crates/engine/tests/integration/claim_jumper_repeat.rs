//! Runtime regression for `RepeatContinuation::WhileCondition` with a BOUNDED
//! ("once") cap and an interactive (library-search) iteration body, via Claim
//! Jumper — "When this creature enters, if an opponent controls more lands than
//! you, you may search your library for a Plains card and put it onto the
//! battlefield tapped. Then if an opponent controls more lands than you, repeat
//! this process once. If you search your library this way, shuffle." (CR 608.2c,
//! CR 608.2d).
//!
//! Exercises the paused-iteration resume path (`drain_pending_repeat_until`):
//! the search parks a `WaitingFor`, the controller submits the real
//! `GameAction`, and only then does the loop re-evaluate the condition and run
//! the single permitted repeat. Discriminating assertions: with the opponent
//! holding two lands and the controller none, the process runs twice (two
//! Plains enter), capped at one repeat, and the "you may" is asked again on the
//! second pass (the ordered prompt trace is `[Optional, Search, Optional,
//! Search]`). Reverting `WhileCondition` collapses it to a single search; a
//! declined pass is still followed by the repeat check (the loop predicate
//! decides, a decline is not a special loop end).
//!
//! Every row drives the parsed trigger body through `resolve_ability_chain` at
//! depth 0 (the call the stack resolver makes) and answers the real
//! `GameAction`s. Rows C9, C11, C12 and C13 graft a synthetic instrument or
//! branch onto the body (labelled at each row); every other row is the printed
//! card unchanged. Shuffle events are never asserted (they are not part of the
//! repeated process).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, QuantityExpr, RepeatContinuation, ResolvedAbility, SubAbilityLink,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::{
    AutoMayChoice, GameState, MayTriggerAutoChoiceKey, MayTriggerAutoChoiceScope, MayTriggerOrigin,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const CLAIM_JUMPER_ORACLE: &str = "Vigilance\nWhen this creature enters, if an opponent controls more lands than you, you may search your library for a Plains card and put it onto the battlefield tapped. Then if an opponent controls more lands than you, repeat this process once. If you search your library this way, shuffle.";

fn claim_jumper_trigger_ability(source: ObjectId) -> ResolvedAbility {
    let parsed = parse_oracle_text(
        CLAIM_JUMPER_ORACLE,
        "Claim Jumper",
        &["Vigilance".to_string()],
        &["Creature".to_string()],
        &[],
    );
    let def = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.clone())
        .expect("Claim Jumper's enters trigger has an execute body");
    assert!(
        matches!(
            def.repeat_until,
            Some(RepeatContinuation::WhileCondition {
                max_iterations: Some(1),
                ..
            })
        ),
        "precondition: parsed trigger carries a bounded (once) WhileCondition, got {:?}",
        def.repeat_until
    );
    assert!(
        def.optional,
        "precondition: the loop root is the optional \"you may search\" instruction"
    );
    build_resolved_from_def(&def, source, P0)
}

/// Add a Plains land card (Land + Plains subtype) to P0's library.
fn add_plains_to_library(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Plains".to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.supertypes.push(Supertype::Basic);
    obj.card_types.subtypes.push("Plains".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Add the Claim Jumper source creature to P0's battlefield.
fn add_claim_jumper_source(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Claim Jumper".to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    id
}

fn plains_on_battlefield(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Plains")
        })
        .count()
}

/// How the controller's "you may" is answered.
#[derive(Clone, Copy, PartialEq)]
enum Gate {
    /// An `OptionalEffectChoice` is raised and answered from the script.
    Prompted,
    /// A stored "always accept" answer: no prompt.
    StoredAccept,
    /// A stored "always decline" answer: no prompt.
    StoredDecline,
}

/// A synthetic graft onto the printed trigger body (labelled in each row).
#[derive(Clone, Copy, PartialEq)]
enum Graft {
    None,
    /// `else_ability = GainLife 1` on the root: each declined pass runs it, and
    /// it replaces the shuffle tail on that branch, so life gained counts
    /// declined passes without touching any shuffle event.
    DeclineGainLife,
    /// An unconditional `GainLife 1` sibling at the end of the chain, so life
    /// gained counts completed passes.
    TailGainLife,
    /// `else_ability = Scry 1` on the root: a decline branch that pauses.
    DeclineScry,
}

struct Outcome {
    plains: usize,
    trace: String,
    life_gained: i32,
    idle: bool,
}

/// Drive every parked choice: answer each `OptionalEffectChoice` from `answers`
/// (`'A'` accept / `'D'` decline; the last character repeats), select nothing at
/// a scry and the first legal card at each search. Returns the ordered prompt
/// trace (`O` optional, `S` search, `Y` scry).
fn drive(runner: &mut GameRunner, answers: &str) -> String {
    let answers: Vec<char> = answers.chars().collect();
    let mut asked = 0usize;
    let mut trace = String::new();
    for _ in 0..60 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                let answer = answers[asked.min(answers.len() - 1)];
                asked += 1;
                trace.push('O');
                Some(GameAction::DecideOptionalEffect {
                    accept: answer == 'A',
                })
            }
            WaitingFor::ScryChoice { .. } => {
                trace.push('Y');
                Some(GameAction::SelectCards { cards: vec![] })
            }
            WaitingFor::SearchChoice { cards, .. } => {
                trace.push('S');
                cards
                    .first()
                    .copied()
                    .map(|c| GameAction::SelectCards { cards: vec![c] })
            }
            _ => None,
        };
        match action {
            Some(a) => {
                runner
                    .act(a)
                    .expect("search/optional/scry choice should resolve");
            }
            None => return trace,
        }
    }
    panic!("did not reach priority after draining choices");
}

fn gain_one_life(source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )
}

fn set_cap(ability: &mut ResolvedAbility, cap: Option<u32>) {
    match ability.repeat_until.as_mut() {
        Some(RepeatContinuation::WhileCondition { max_iterations, .. }) => *max_iterations = cap,
        other => panic!("expected a WhileCondition loop, got {other:?}"),
    }
}

/// Store the controller's answer to the root's "you may" and key the ability to it.
fn store_answer(
    runner: &mut GameRunner,
    ability: &mut ResolvedAbility,
    source: ObjectId,
    choice: AutoMayChoice,
) {
    let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
    runner.state_mut().set_may_trigger_auto_choice(
        MayTriggerAutoChoiceKey {
            player: P0,
            source_id: source,
            origin: origin.clone(),
        },
        choice,
    );
    ability.set_may_trigger_origin_recursive(origin);
}

/// Run Claim Jumper's trigger body: the opponent holds `opp_lands` basic lands,
/// the controller none, and the controller's library `plains` Plains.
fn run(
    opp_lands: usize,
    plains: usize,
    answers: &str,
    gate: Gate,
    graft: Graft,
    cap: Option<Option<u32>>,
) -> Outcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..opp_lands {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let mut runner = scenario.build();
    let source = add_claim_jumper_source(&mut runner);
    for _ in 0..plains {
        add_plains_to_library(&mut runner);
    }
    let mut ability = claim_jumper_trigger_ability(source);
    if let Some(cap) = cap {
        set_cap(&mut ability, cap);
    }
    match graft {
        Graft::None => {}
        Graft::DeclineGainLife => {
            ability.else_ability = Some(Box::new(gain_one_life(source)));
        }
        Graft::DeclineScry => {
            ability.else_ability = Some(Box::new(ResolvedAbility::new(
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                source,
                P0,
            )));
        }
        Graft::TailGainLife => {
            let mut tail = gain_one_life(source);
            tail.sub_link = SubAbilityLink::SequentialSibling;
            let mut cur = &mut ability;
            while cur.sub_ability.is_some() {
                cur = cur.sub_ability.as_mut().unwrap();
            }
            cur.sub_ability = Some(Box::new(tail));
        }
    }
    match gate {
        Gate::Prompted => {}
        Gate::StoredAccept => {
            store_answer(&mut runner, &mut ability, source, AutoMayChoice::Accept)
        }
        Gate::StoredDecline => {
            store_answer(&mut runner, &mut ability, source, AutoMayChoice::Decline)
        }
    }
    let life_before = runner.state().players[0].life;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();
    let trace = drive(&mut runner, answers);
    let state = runner.state();
    Outcome {
        plains: plains_on_battlefield(state),
        trace,
        life_gained: state.players[0].life - life_before,
        idle: state.resolution_stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. }),
    }
}

/// A prompted run of the printed card.
fn prompted(opp_lands: usize, plains: usize, answers: &str) -> Outcome {
    run(
        opp_lands,
        plains,
        answers,
        Gate::Prompted,
        Graft::None,
        None,
    )
}

fn assert_outcome(o: &Outcome, plains: usize, trace: &str, what: &str) {
    assert_eq!(o.plains, plains, "{what}: Plains on the battlefield");
    assert_eq!(o.trace, trace, "{what}: ordered prompt trace");
    assert!(
        o.idle,
        "{what}: the resolution stack must be empty at priority (no stranded loop frame)"
    );
}

#[test]
fn claim_jumper_repeats_search_once_while_opponent_has_more_lands() {
    // C1: CR 608.2c + CR 608.2d. Opponent controls two lands; controller starts
    // with none, so the intervening-if and the repeat predicate both hold. The
    // first search puts a Plains (1 land), the opponent is still ahead, so the
    // once-capped repeat runs the process again, "you may" included: two
    // prompts, two searches. The condition is then false (2 == 2).
    let o = prompted(2, 8, "AA");
    assert_outcome(&o, 2, "OSOS", "accept, accept at opponent 2");
}

#[test]
fn claim_jumper_declining_the_repeat_ends_after_one_plains() {
    // C2: the second pass's "you may" is its own decision. Paired positive: the
    // first search happened (trace and Plains), so the decline is pass two's.
    for opp in [2, 3] {
        let o = prompted(opp, 8, "AD");
        assert_outcome(&o, 1, "OSO", &format!("accept, decline at opponent {opp}"));
    }
}

#[test]
fn claim_jumper_once_is_two_passes_in_total_never_three() {
    // C3: with the opponent three or four lands ahead the condition is still
    // true after both passes; only the cap stops it. (Base parked a duplicate
    // copy of the loop and fetched a third Plains.)
    for opp in [3, 4] {
        let o = prompted(opp, 8, "AA");
        assert_outcome(&o, 2, "OSOS", &format!("accept, accept at opponent {opp}"));
    }
}

#[test]
fn claim_jumper_declined_first_pass_is_still_followed_by_the_repeat_check() {
    // C4: CR 608.2c. "Then if an opponent controls more lands than you, repeat
    // this process once" has no "if you do": after a declined first pass
    // nothing changed, the condition holds, and the "you may" is offered again.
    let o = prompted(2, 8, "DD");
    assert_outcome(&o, 0, "OO", "decline, decline at opponent 2");
}

#[test]
fn claim_jumper_decline_then_accept_fetches_one_plains() {
    // C5: pass two is offered after a declined pass one and accepted.
    for opp in [2, 3] {
        let o = prompted(opp, 8, "DA");
        assert_outcome(&o, 1, "OOS", &format!("decline, accept at opponent {opp}"));
    }
}

#[test]
fn claim_jumper_the_cap_bounds_declines_too() {
    // C6: a third offer is never made, however far ahead the opponent is.
    let o = prompted(3, 8, "DDD");
    assert_outcome(&o, 0, "OO", "decline, decline, decline at opponent 3");
}

#[test]
fn claim_jumper_does_not_repeat_when_lands_equalize_after_first_search() {
    // C7: opponent controls exactly one land; controller none. The first search
    // puts a Plains (controller -> 1 land == opponent), so the repeat predicate
    // is false and the process does NOT run a second time. Sibling/negative
    // case proving the condition gate discriminates against the bounded cap
    // alone (a regression guard, not discriminating by design).
    let o = prompted(1, 8, "A");
    assert_outcome(&o, 1, "OS", "accept at opponent 1");
}

#[test]
fn claim_jumper_stored_accept_runs_two_passes_with_no_prompt_and_no_stranded_frame() {
    // C8: a stored "always accept" is the controller's answer given without a
    // prompt: the Plains are those of the prompted run, nothing is asked, and
    // no copy of the loop is left on the resolution stack. (The gate-keyed
    // early return is what stops the duplicate copy and the stranded frame.)
    for opp in [2, 3, 4] {
        let o = run(opp, 8, "A", Gate::StoredAccept, Graft::None, None);
        assert_outcome(&o, 2, "SS", &format!("stored accept at opponent {opp}"));
    }
    let o = run(1, 8, "A", Gate::StoredAccept, Graft::None, None);
    assert_outcome(&o, 1, "S", "stored accept at opponent 1");
}

#[test]
fn claim_jumper_stored_decline_runs_two_passes_with_no_prompt() {
    // C9. SYNTHETIC instrument: `else_ability = GainLife 1` on the root, which
    // each declined pass runs, so life gained counts passes and no Shuffle event
    // is involved. The cap-0 run is the one-pass reference: it must gain exactly
    // 1 before the default-cap count is meaningful (the instrument's reach-guard).
    let reference = run(
        2,
        8,
        "A",
        Gate::StoredDecline,
        Graft::DeclineGainLife,
        Some(Some(0)),
    );
    assert_eq!(
        reference.life_gained, 1,
        "reach-guard: one declined pass gains exactly 1 life"
    );
    assert_outcome(&reference, 0, "", "stored decline, one-pass reference");

    for opp in [1, 2, 3] {
        let o = run(
            opp,
            8,
            "A",
            Gate::StoredDecline,
            Graft::DeclineGainLife,
            None,
        );
        assert_eq!(
            o.life_gained, 2,
            "stored decline at opponent {opp}: two passes (the decline is followed by the repeat check, capped at once)"
        );
        assert_outcome(&o, 0, "", &format!("stored decline at opponent {opp}"));
    }

    // Prompted cross-check of the same graft.
    let o = run(2, 8, "DD", Gate::Prompted, Graft::DeclineGainLife, None);
    assert_eq!(o.life_gained, 2, "prompted decline, decline at opponent 2");
    assert_outcome(&o, 0, "OO", "prompted decline, decline at opponent 2");
    let o = run(3, 8, "DDD", Gate::Prompted, Graft::DeclineGainLife, None);
    assert_eq!(o.life_gained, 2, "prompted D,D,D at opponent 3");
    assert_outcome(&o, 0, "OO", "prompted D,D,D at opponent 3");

    // The printed card, no graft: no Plains, nothing asked, nothing stranded.
    let o = run(2, 8, "A", Gate::StoredDecline, Graft::None, None);
    assert_outcome(&o, 0, "", "printed card, stored decline");
}

#[test]
fn claim_jumper_with_nothing_to_find_still_asks_on_each_pass() {
    // C10: pass one is offered and accepted, there is no Plains to find (no
    // search prompt is raised for an empty match set), the repeat condition is
    // still true, pass two is offered and the cap is then spent. The values are
    // derived from the printed text; the base agrees only by accident (its
    // second prompt is the duplicate copy of the loop).
    for opp in [2, 3] {
        let o = prompted(opp, 0, "AA");
        assert_outcome(
            &o,
            0,
            "OO",
            &format!("no Plains, accept, accept at opponent {opp}"),
        );
        let o = prompted(opp, 0, "AD");
        assert_outcome(
            &o,
            0,
            "OO",
            &format!("no Plains, accept, decline at opponent {opp}"),
        );
    }
    // Sibling: with exactly one Plains the prompt appears for the pass that can
    // find it.
    let o = prompted(2, 1, "AA");
    assert_outcome(&o, 1, "OSO", "one Plains, accept, accept at opponent 2");
}

#[test]
fn claim_jumper_stored_accept_with_nothing_to_find_runs_the_nested_loop_once() {
    // C11. SYNTHETIC instrument: an unconditional `GainLife 1` sibling appended
    // at the end of the chain, so life gained counts completed passes. The cap-0
    // run is the one-pass reference and must gain exactly 1 first.
    let reference = run(
        2,
        0,
        "A",
        Gate::StoredAccept,
        Graft::TailGainLife,
        Some(Some(0)),
    );
    assert_eq!(
        reference.life_gained, 1,
        "reach-guard: one pass gains exactly 1 life"
    );
    for opp in [1, 2, 3] {
        let o = run(opp, 0, "A", Gate::StoredAccept, Graft::TailGainLife, None);
        assert_eq!(
            o.life_gained, 2,
            "stored accept with nothing to find at opponent {opp}: two passes, not four"
        );
        assert_outcome(
            &o,
            0,
            "",
            &format!("stored accept, no Plains, opponent {opp}"),
        );
    }
    // Guard: with Plains to find the same graft runs both passes and finds both.
    let o = run(2, 8, "A", Gate::StoredAccept, Graft::TailGainLife, None);
    assert_eq!(o.life_gained, 2, "eight Plains: two passes");
    assert_outcome(&o, 2, "SS", "stored accept, eight Plains, tail graft");
}

/// A hand-built `WhileCondition` root whose "you may" the engine auto-declines
/// as infeasible (an empty hand), with an unconditional `GainLife 1` sibling the
/// decline runs. SYNTHETIC: no printed card.
fn infeasible_optional_root(source: ObjectId, cap: u32) -> ResolvedAbility {
    let parsed = parse_oracle_text(
        "Trample\nWhen this creature enters, you may put a card from your hand on the bottom of your library.",
        "Cultivator Colossus",
        &["Trample".to_string()],
        &["Creature".to_string()],
        &[],
    );
    let def = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.clone())
        .expect("the enters trigger has an execute body");
    let mut root = build_resolved_from_def(&def, source, P0);
    assert!(root.optional, "precondition: the root is optional");
    assert!(
        matches!(root.effect, Effect::PutAtLibraryPosition { .. }),
        "precondition: the root effect is PutAtLibraryPosition, got {:?}",
        root.effect
    );
    root.repeat_until = Some(RepeatContinuation::WhileCondition {
        condition: Box::new(AbilityCondition::IsYourTurn),
        max_iterations: Some(cap),
    });
    let mut gain = gain_one_life(source);
    gain.sub_link = SubAbilityLink::SequentialSibling;
    root.sub_ability = Some(Box::new(gain));
    root
}

#[test]
fn infeasible_optional_root_keeps_the_live_driver_as_the_loop_continuation() {
    // C12: an optional root auto-declined as infeasible declines inside the
    // driver's own pass, and the live driver is the continuation; the decline
    // arm must not also re-enter (that would run every pass twice).
    // Passes are cap + 1 (the invariant of "repeat this process [once]").
    for cap in [1u32, 3] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();
        let source = add_claim_jumper_source(&mut runner);
        let root = infeasible_optional_root(source, cap);
        let before = runner.state().players[0].life;
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &root, &mut events, 0).unwrap();
        let state = runner.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "cap {cap}: no OptionalEffectChoice was raised (the decline was automatic), got {:?}",
            state.waiting_for
        );
        assert!(
            state.resolution_stack.is_empty(),
            "cap {cap}: nothing stranded"
        );
        assert_eq!(
            state.players[0].life - before,
            cap as i32 + 1,
            "cap {cap}: passes are cap + 1"
        );
    }
}

#[test]
fn claim_jumper_decline_branch_that_pauses_is_resumed_then_the_repeat_check_runs() {
    // C13. SYNTHETIC: `else_ability = Scry 1` on the root, a decline branch that
    // pauses. CR 608.2c: the declined instruction's own follow-up runs first,
    // then the repeat check.
    let o = run(2, 8, "DA", Gate::Prompted, Graft::DeclineScry, None);
    assert_outcome(&o, 1, "OYOS", "decline (scry), accept");
    let o = run(2, 8, "DD", Gate::Prompted, Graft::DeclineScry, None);
    assert_outcome(&o, 0, "OYOY", "decline (scry), decline (scry)");
}

// Production ETB consumers; the direct-body rows above supplement these
// with synthetic cap/tail/decline instruments. Regenerate the compressed
// integration CardDB fixture from the fresh production export with its generator.
fn real_claim_jumper(opponent_lands: usize, library: &[&str]) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("production repeat fixtures must load");
    assert_eq!(
        db.get_face_by_name("Claim Jumper")
            .unwrap()
            .oracle_text
            .as_deref(),
        Some(CLAIM_JUMPER_ORACLE)
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(P0, "Claim Jumper", Zone::Hand, db);
    for _ in 0..opponent_lands {
        scenario.add_real_card(P1, "Island", Zone::Battlefield, db);
    }
    for name in library {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    scenario.with_mana_pool(
        P0,
        (0..10)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.cast(source).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    if opponent_lands > 0 {
        // CR 603.4: the actual entry satisfied the intervening-if clause.
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack.last().unwrap().source_id, source);
        runner.resolve_top();
    } else {
        assert!(
            runner.state().stack.is_empty(),
            "equal counts prevent the ETB trigger"
        );
    }
    (runner, source)
}

fn assert_claim_prompt(runner: &GameRunner, source: ObjectId) -> MayTriggerAutoChoiceKey {
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice {
            player,
            source_id,
            may_trigger_key: Some(key),
            ..
        } => {
            assert_eq!((*player, *source_id), (P0, source));
            assert_eq!((key.player, key.source_id), (P0, source));
            assert!(matches!(key.origin, MayTriggerOrigin::Definition { .. }));
            key.clone()
        }
        other => panic!("expected the live Claim Jumper optional offer: {other:?}"),
    }
}

fn assert_claim_idle(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

#[test]
fn claim_jumper_real_etb_once_cap_and_decline_matrix() {
    for (answers, expected_trace, plains) in [
        ("AA", "OSOS", 2),
        ("AD", "OSO", 1),
        ("DA", "OOS", 1),
        ("DD", "OO", 0),
    ] {
        let (mut runner, source) = real_claim_jumper(4, &["Plains", "Plains", "Plains"]);
        let key = assert_claim_prompt(&runner, source);
        let mut trace = String::new();
        for answer in answers.chars() {
            assert_eq!(
                assert_claim_prompt(&runner, source),
                key,
                "both passes retain the same trigger origin"
            );
            trace.push('O');
            runner
                .act(GameAction::DecideOptionalEffect {
                    accept: answer == 'A',
                })
                .unwrap();
            if answer == 'A' {
                let chosen = match &runner.state().waiting_for {
                    WaitingFor::SearchChoice { player, cards, .. } => {
                        assert_eq!(*player, P0);
                        assert!(!cards.is_empty());
                        cards[0]
                    }
                    other => panic!("an accepted search must pause: {other:?}"),
                };
                trace.push('S');
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .unwrap();
                assert_eq!(runner.state().objects[&chosen].zone, Zone::Battlefield);
                assert!(runner.state().objects[&chosen].tapped);
            }
        }
        // CR 608.2c: "once" is one additional pass even while four opposing
        // lands keep the live predicate true after both successful searches.
        assert_eq!(trace, expected_trace);
        assert_eq!(plains_on_battlefield(runner.state()), plains);
        assert_claim_idle(&runner);
    }
    let (mut equality, _) = real_claim_jumper(1, &["Plains", "Plains"]);
    assert_eq!(drive(&mut equality, "A"), "OS");
    assert_eq!(plains_on_battlefield(equality.state()), 1);
    assert_claim_idle(&equality);
    let (equal_initially, _) = real_claim_jumper(0, &["Plains", "Plains"]);
    assert_eq!(plains_on_battlefield(equal_initially.state()), 0);
    assert_claim_idle(&equal_initially);
}

#[test]
fn claim_jumper_real_etb_empty_search_still_checks_repeat_and_cap() {
    for library in [vec![], vec!["Shock"], vec!["Plains", "Plains"]] {
        let (mut runner, source) = real_claim_jumper(4, &library);
        for _ in 0..2 {
            assert_claim_prompt(&runner, source);
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            if library.first() == Some(&"Plains") {
                match &runner.state().waiting_for {
                    WaitingFor::SearchChoice { player, cards, .. } => {
                        assert_eq!(*player, P0);
                        assert_eq!(cards.len(), 2);
                    }
                    other => {
                        panic!("matching Plains produce the public fail-to-find choice: {other:?}")
                    }
                }
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .unwrap();
            }
        }
        assert_eq!(plains_on_battlefield(runner.state()), 0);
        assert_claim_idle(&runner);
    }
}

#[test]
fn claim_jumper_real_etb_remembered_decline_reused_within_trigger() {
    let (mut runner, source) = real_claim_jumper(4, &["Plains", "Plains", "Plains"]);
    let key = assert_claim_prompt(&runner, source);
    assert_eq!(
        runner.state().players[0].library.len(),
        3,
        "feasible search, with the repeat predicate still true"
    );
    runner
        .act(GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Decline,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        })
        .unwrap();
    // CR 603.5 + CR 608.2c: the independent repeat still follows the first
    // decline; its unchanged trigger identity reuses the remembered answer.
    assert_eq!(
        runner.state().may_trigger_auto_choice_for_live_prompt(&key),
        Some(AutoMayChoice::Decline)
    );
    assert_eq!(plains_on_battlefield(runner.state()), 0);
    assert_eq!(runner.state().players[0].library.len(), 3);
    assert_claim_idle(&runner);
}
