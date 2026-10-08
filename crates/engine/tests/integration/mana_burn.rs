//! Mana burn — the pre-M10 rule a custom format can opt back into.
//!
//! The current rules have no such rule; the glossary entry "Mana Burn
//! (Obsolete)" records that "unspent mana caused a player to lose life."
//! Two things separate it from the modern behavior, and both are asserted
//! here against a real phase advance rather than against the helpers:
//!
//! 1. **Pools empty at the end of a PHASE (CR 500.1's five), not every step.**
//!    Modern CR 106.4 / CR 500.5 empty at the end of each step AND phase.
//! 2. **Emptying costs life** equal to the mana lost.
//!
//! Every assertion is paired against the same scenario under a modern format,
//! so a failure to burn and a failure to set the scenario up are
//! distinguishable.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PlayerFilter, QuantityExpr, QuantityModification,
    ReplacementDefinition, ReplacementPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::custom_format::{old_school_93_94, swedish_old_school};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;

use super::yurlok_of_scorch_thrash::add_yurlok;

const POOL: usize = 2;

fn pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(9_001), false, vec![]); count]
}

/// A game sitting in the upkeep step with `POOL` unspent mana, under `format`.
///
/// Upkeep is chosen deliberately: it is inside the beginning phase (CR 501.1)
/// alongside untap and draw, so the untap → upkeep → draw run exercises steps
/// the OLD `EndOfCombat` retention could never have covered. A combat-only
/// implementation would pass a combat-based test while failing this one.
fn game_in_upkeep_with_mana(format: FormatConfig) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    // CR 704.5b: the draw step draws a card, and an empty library loses the
    // game — which ends the turn before any phase boundary is reached. Stocking
    // the library keeps the game alive long enough to observe the boundary.
    scenario.with_library_top(P0, &["Mountain", "Mountain", "Mountain"]);
    scenario.with_mana_pool(P0, pool(POOL));
    let mut runner = scenario.build();
    runner.state_mut().format_config = format;
    runner
}

fn unspent(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("player 0 exists")
        .mana_pool
        .total()
}

/// Old School 93/94 declares `mana_burn: Obsolete`, so this is the shipped
/// preset's own behavior, not a synthetic config.
#[test]
fn mana_survives_steps_inside_a_phase_and_burns_when_the_phase_ends() {
    let format = FormatConfig::for_custom_rules(&old_school_93_94().rules);
    let mut runner = game_in_upkeep_with_mana(format);
    let life_before = runner.life(P0);
    assert_eq!(unspent(&runner), POOL, "scenario starts with unspent mana");

    // Upkeep -> Draw: a step boundary INSIDE the beginning phase (CR 501.1).
    // Modern rules would empty the pool here; the pre-M10 rule does not.
    runner.advance_to_phase(Phase::Draw);
    assert_eq!(runner.state().phase, Phase::Draw);
    assert_eq!(
        unspent(&runner),
        POOL,
        "mana must survive a step boundary within one phase"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "no life is lost until the phase itself ends"
    );

    // Draw -> PreCombatMain: a real CR 500.1 phase crossing. The pool empties
    // and the emptied count is the life lost.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "did not reach the phase boundary; halted waiting on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        unspent(&runner),
        0,
        "the pool empties at the phase boundary"
    );
    assert_eq!(
        runner.life(P0),
        life_before - POOL as i32,
        "mana burn costs one life per unspent mana"
    );

    // NOTE: the `GameEvent::ManaBurn` narration is deliberately NOT asserted
    // here. `advance_to_phase` discards the events it drives, and the log is
    // carried on `ActionResult` rather than `GameState`, so there is no honest
    // way to observe it from this harness — asserting something weaker and
    // calling it event coverage would be worse than saying so. Life total and
    // pool contents below are the player-visible behavior either way.
}

/// The paired control: the same scenario under a modern format. Without this,
/// every assertion above would still pass against an engine that emptied pools
/// at the wrong time or never emptied them at all.
#[test]
fn a_modern_format_empties_every_step_and_costs_no_life() {
    let mut runner = game_in_upkeep_with_mana(FormatConfig::standard());
    let life_before = runner.life(P0);

    runner.advance_to_phase(Phase::Draw);
    assert_eq!(
        unspent(&runner),
        0,
        "CR 106.4 / CR 500.5: modern pools empty at the end of every step"
    );
    assert_eq!(runner.life(P0), life_before, "emptying costs nothing");

    // The same second advance the mana-burn test makes. If the harness cannot
    // cross this boundary even with an empty pool, that is a scenario-driver
    // limitation and not a mana-burn defect — this is what tells them apart.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "halted waiting on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0), life_before);
}

/// A custom format that does NOT declare the axis behaves like a built-in.
/// Swedish Old School is the shipped preset that proves it: an old card pool
/// played under fully modern rules, which is exactly what its source says.
#[test]
fn a_custom_format_without_the_axis_does_not_burn() {
    let format = FormatConfig::for_custom_rules(&swedish_old_school().rules);
    let mut runner = game_in_upkeep_with_mana(format);
    let life_before = runner.life(P0);

    runner.advance_to_phase(Phase::Draw);
    assert_eq!(
        unspent(&runner),
        0,
        "a custom format is not automatically an old-rules format"
    );
    assert_eq!(runner.life(P0), life_before);
}

/// Diagnostic: is the missing cleanup-exit drain pre-existing, or introduced by
/// mana burn? `EndOfTurn` retention (Klauth) has nothing to do with this PR —
/// its marker is cleared by the cleanup action itself, leaving an ordinary
/// unspent unit that the ending phase should empty. Under a MODERN format.
#[test]
fn diagnostic_end_of_turn_mana_does_not_survive_into_the_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["Mountain", "Mountain", "Mountain"]);
    let mut retained = ManaUnit::new(ManaType::Red, ObjectId(9_002), false, vec![]);
    retained.expiry = Some(engine::types::mana::ManaExpiry::EndOfTurn);
    scenario.with_mana_pool(P0, vec![retained]);
    let mut runner = scenario.build();
    runner.state_mut().format_config = FormatConfig::standard();

    let turn_before = runner.state().turn_number;
    runner.advance_to_phase(Phase::Untap);
    assert_ne!(
        runner.state().turn_number,
        turn_before,
        "the turn must actually roll over; halted on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        unspent(&runner),
        0,
        "CR 106.4: the ending phase must empty the pool before the next turn"
    );
}

/// The review's HIGH finding, tested directly: mana produced during the END
/// step is retained across End -> Cleanup (both inside CR 512.1's ending
/// phase), so the ending phase's own boundary — Cleanup -> Untap — is the one
/// that must empty it and charge the burn.
#[test]
fn mana_held_through_the_ending_phase_burns_before_the_next_turn() {
    let format = FormatConfig::for_custom_rules(&old_school_93_94().rules);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["Mountain", "Mountain", "Mountain"]);
    scenario.with_mana_pool(P0, pool(POOL));
    let mut runner = scenario.build();
    runner.state_mut().format_config = format;

    let life_before = runner.life(P0);
    let turn_before = runner.state().turn_number;
    assert_eq!(unspent(&runner), POOL);

    runner.advance_to_phase(Phase::Untap);
    assert_ne!(
        runner.state().turn_number,
        turn_before,
        "the turn must roll over; halted on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        unspent(&runner),
        0,
        "mana must not survive the ending phase into the next turn"
    );
    assert_eq!(
        runner.life(P0),
        life_before - POOL as i32,
        "the ending phase's boundary charges the burn like any other"
    );
}

// ---------------------------------------------------------------------------
// Provenance through the CR 616.1 replacement pipeline.
//
// A mana burn is a life loss like any other, so it can be doubled, prevented
// or substituted. Whatever survives that pipeline, the log still has to say
// mana burn is WHY — and it must never say so about a loss that is not one.
// These two are the paired halves of that: the burn that happens must be
// narrated, and the burn that does not happen must not leave its name behind.
// ---------------------------------------------------------------------------

/// Every `ManaBurn` event in `events`, as `(player, amount)`.
fn burns(events: &[GameEvent]) -> Vec<(PlayerId, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaBurn { player_id, amount } => Some((*player_id, *amount)),
            _ => None,
        })
        .collect()
}

/// A branch prompt, used as the substitute effect so the CR 614.6 continuation
/// pauses on real player input instead of completing synchronously.
fn gain_branches() -> Effect {
    let gain = |amount| {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
        )
    };
    Effect::ChooseOneOf {
        chooser: PlayerFilter::Controller,
        branches: vec![gain(1), gain(2)],
    }
}

/// A modifier of an opponent's life loss.
fn opponent_loss_modifier(
    modification: QuantityModification,
    description: &str,
) -> ReplacementDefinition {
    let mut def = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(modification)
        .description(description.to_string());
    def.valid_player = Some(ReplacementPlayerScope::Opponent);
    def
}

const DOUBLED_WITH_SUBSTITUTE: &str = "Double, then choose a gain";

/// Doubles an opponent's life loss, then runs `gain_branches` — so the loss is
/// applied before the substitute hands control to a prompt.
fn doubled_with_substitute() -> ReplacementDefinition {
    opponent_loss_modifier(QuantityModification::DOUBLE, DOUBLED_WITH_SUBSTITUTE)
        .execute(AbilityDefinition::new(AbilityKind::Spell, gain_branches()))
}

/// A two-player game in the precombat main phase under Old School 93/94, with
/// `P1` holding `POOL` unspent mana. `P0` hosts the replacement effects, so
/// `ReplacementPlayerScope::Opponent` names `P1` and nothing else.
fn burner_facing_replacements(defs: Vec<ReplacementDefinition>) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 51);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, pool(POOL));
    let mut creature = scenario.add_creature(P0, "Burn Replacements", 1, 1);
    for def in defs {
        creature.with_replacement_definition(def);
    }
    let mut runner = scenario.build();
    runner.state_mut().format_config = FormatConfig::for_custom_rules(&old_school_93_94().rules);
    runner
}

/// CR 614.6: the burn RESOLVED and then its substitute paused, so the loss is
/// already final — the log must name its cause even though the transition has
/// not finished.
///
/// Reverts to red by dropping the amount from
/// `ReplacementDeferred::SubstitutionContinuation`: the drain then has no
/// figure to narrate at the point the pause happens, the root's provenance is
/// parked instead, and no later resume ever claims it — `LifeChanged` lands
/// with no `ManaBurn` beside it.
#[test]
fn a_burn_whose_substitute_pauses_still_names_itself() {
    let mut runner = burner_facing_replacements(vec![doubled_with_substitute()]);
    let life_before = runner.state().players[1].life;

    let mut events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut events);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "the substitute must pause, or this exercises the synchronous path; \
         got {:?}",
        runner.state().waiting_for
    );
    // The REAL figure, not the pool count: the replacement doubled it.
    let burned = POOL as i32 * 2;
    assert_eq!(runner.state().players[1].life, life_before - burned);
    assert_eq!(
        burns(&events),
        vec![(P1, burned as u32)],
        "the resolved burn is narrated once, with what was actually lost"
    );
    // Narrated, so nothing is parked: a record left here would outlive its
    // event and be inherited by the next same-player loss to resume.
    assert!(
        runner
            .state()
            .pending_phase_transition_progress
            .as_ref()
            .is_some_and(|progress| progress.in_flight_life_loss.is_none()),
        "the paused cursor must hold no provenance for a burn already narrated"
    );

    // Answering the prompt finishes the transition and adds no second burn.
    let resumed = runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("answering the substitute resumes the drain");
    assert!(
        burns(&resumed.events).is_empty(),
        "the burn is emitted once"
    );
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(runner.state().pending_phase_transition_progress.is_none());
}

/// CR 614.1a: a PREVENTED burn is not a burn. Its parked provenance has to be
/// consumed at that terminal outcome, or it outlives its own event and the next
/// same-player life loss to resume through the pipeline inherits it.
///
/// The interactive substitute is what makes the leak observable: it holds the
/// phase transition open past the prevented choice, so the record can be read
/// at the one moment it would still be there.
#[test]
fn a_prevented_burn_leaves_no_provenance_behind() {
    let mut double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .description("Double".to_string());
    double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut substitute = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .execute(AbilityDefinition::new(AbilityKind::Spell, gain_branches()))
        .description("Choose gain instead".to_string());
    substitute.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut runner = burner_facing_replacements(vec![double, substitute]);
    let life_before = runner.state().players[1].life;

    let mut events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut events);

    let substitute_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Choose gain instead")
            .expect("the substituting candidate is offered"),
        waiting => panic!("expected a life-loss replacement choice, got {waiting:?}"),
    };
    let chosen = runner
        .act(GameAction::ChooseReplacement {
            index: substitute_index,
        })
        .expect("the prevented choice surfaces the substitute's own prompt");
    events.extend(chosen.events);

    // The transition is still open, which is the whole point — the parked
    // record would still be readable here if it were not consumed.
    let progress = runner
        .state()
        .pending_phase_transition_progress
        .as_ref()
        .expect("the phase cursor stays owned while the substitute waits");
    assert!(
        progress.in_flight_life_loss.is_none(),
        "a prevented burn must not leave provenance parked, or the next \
         same-player loss to resume is logged as mana burn: {:?}",
        progress.in_flight_life_loss
    );

    let resumed = runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("answering the substitute resumes the drain");
    events.extend(resumed.events);

    assert_eq!(
        runner.state().players[1].life,
        life_before,
        "CR 614.1a: the prevented burn took no life"
    );
    assert!(
        burns(&events).is_empty(),
        "nothing may be narrated as mana burn when no burn resolved: {:?}",
        burns(&events)
    );
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(runner.state().phase, Phase::BeginCombat);
}

/// Answer the paused ordering choice with the replacement named `description`.
fn choose_replacement(runner: &mut GameRunner, description: &str) -> Vec<GameEvent> {
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == description)
            .unwrap_or_else(|| panic!("no {description:?} replacement candidate")),
        waiting => panic!("expected a life-loss replacement choice, got {waiting:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the replacement choice resolves")
        .events
}

/// Answer the paused `gain_branches` prompt, and prove the phase transition
/// held behind it then completes exactly once, running the substitute once.
fn answer_substitute(runner: &mut GameRunner, mut events: Vec<GameEvent>) -> Vec<GameEvent> {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "the substitute must pause, or this exercises the synchronous path; \
         got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_phase_transition_progress.is_some(),
        "the phase transition must be held behind the substitute's prompt"
    );
    let host_life = runner.life(P0);
    events.extend(
        runner
            .act(GameAction::ChooseBranch { index: 1 })
            .expect("answering the substitute resumes the drain")
            .events,
    );
    assert!(
        runner.state().pending_phase_transition_progress.is_none(),
        "answering the substitute must finish the phase transition, not strand it"
    );
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::BeginCombat
                }
            ))
            .count(),
        1,
        "the phase entry completes exactly once"
    );
    assert_eq!(
        runner.life(P0),
        host_life + 2,
        "the chosen gain runs exactly once"
    );
    events
}

/// CR 616.1: an ordering choice applies nothing until it is answered, so the
/// choice's resume, not the drain, names the loss — and it names what the
/// chosen order produced, not the pool count.
#[test]
fn a_burn_resolved_by_an_ordering_choice_names_the_chosen_loss_once() {
    let mut runner = burner_facing_replacements(vec![
        opponent_loss_modifier(QuantityModification::DOUBLE, "Double"),
        opponent_loss_modifier(QuantityModification::Plus { value: 1 }, "Plus one"),
    ]);
    let life_before = runner.life(P1);

    let mut events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut events);
    events.extend(choose_replacement(&mut runner, "Double"));

    // Double first, then the remaining Plus one.
    let burned = POOL as u32 * 2 + 1;
    assert_eq!(runner.life(P1), life_before - burned as i32);
    assert_eq!(burns(&events), vec![(P1, burned)]);
    assert!(runner.state().pending_phase_transition_progress.is_none());
    assert_eq!(runner.state().phase, Phase::BeginCombat);
}

/// CR 614.6 + CR 500.5: the ordering choice picks a replacement whose
/// substitute pauses only AFTER the choice's resume has applied the loss. The
/// drain may not advance until that substitute finishes, and must finish once
/// it is answered.
///
/// Reverts to red by dropping the `Execute` arm's
/// `mark_phase_transition_awaiting_post_replacement`: the answered prompt then
/// leaves the phase cursor standing, and no resume path ever drains it.
#[test]
fn a_burn_whose_chosen_replacement_substitute_pauses_still_finishes_the_phase() {
    let mut runner = burner_facing_replacements(vec![
        doubled_with_substitute(),
        opponent_loss_modifier(QuantityModification::Plus { value: 1 }, "Plus one"),
    ]);
    let life_before = runner.life(P1);

    let mut events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut events);
    events.extend(choose_replacement(&mut runner, DOUBLED_WITH_SUBSTITUTE));
    let events = answer_substitute(&mut runner, events);

    // The chosen Double first, then the remaining Plus one.
    let burned = POOL as u32 * 2 + 1;
    assert_eq!(runner.life(P1), life_before - burned as i32);
    assert_eq!(burns(&events), vec![(P1, burned)]);
}

/// The `UnspentManaStatic` control on the substitute path: the same paused
/// substitute, applied to a Yurlok loss under a modern format. That loss is a
/// card's doing, not mana burn, so its `LifeChanged` says everything and no
/// `ManaBurn` may name it. The Yurlok suite drives this path but asserts
/// nothing about `ManaBurn`, so it cannot stand in for this.
#[test]
fn an_unspent_mana_static_loss_through_a_paused_substitute_is_not_mana_burn() {
    let mut scenario = GameScenario::new_n_player(2, 51);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, pool(POOL));
    scenario
        .add_creature(P0, "Burn Replacements", 1, 1)
        .with_replacement_definition(doubled_with_substitute());
    add_yurlok(&mut scenario);
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    let mut events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut events);
    let events = answer_substitute(&mut runner, events);

    assert_eq!(
        runner.life(P1),
        life_before - POOL as i32 * 2,
        "reach guard: the Yurlok loss really resolved through the paused substitute"
    );
    assert!(burns(&events).is_empty());
}
