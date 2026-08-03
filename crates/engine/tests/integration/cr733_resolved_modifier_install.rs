//! CR733 P2 coverage for the two modifier-installation families.
//!
//! Both families install a modifier that outlives the effect that created it,
//! and both wrote their mutation raw before this change, so a retained-prefix
//! replay had no record that the modifier was ever created.
//!
//! They are deliberately TWO command variants rather than one parameterized
//! variant, because the parameterization axis would straddle two CR sections the
//! engine resolves through entirely separate machinery:
//!
//! - A delayed triggered ability is CR 603.7. It never touches the CR 613 layer
//!   system; it waits in `delayed_triggers` until its condition occurs, then goes
//!   on the stack as an ordinary triggered ability (CR 603.7b). It draws NO
//!   allocator value.
//! - A transient continuous effect is CR 611.2a. It never uses the stack; it
//!   applies continuously through the CR 613 layers until its duration ends. It
//!   draws TWO allocator values — an effect id, and a CR 613.7b timestamp that
//!   orders it within its layer.
//!
//! Their `expected_*`/`resulting_*` shapes therefore differ in kind, not in a
//! leaf value: one command has an allocator receipt to verify and the other has
//! nothing to verify. Collapsing them would put two unrelated invariants behind
//! one validator arm and one applier, which is the "categorical boundary"
//! failure CLAUDE.md warns about, not the sibling-cluster smell it warns about.
//!
//! Both tests drive the REAL pipeline: a verbatim-Oracle spell cast from hand and
//! resolved off the stack, never a direct call to the authority.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::DelayedTriggerCondition;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedContinuousEffectReplayInvariantError, ResolvedDelayedTriggerReplayInvariantError,
    ResolvedRulesCommand,
};

/// Verbatim Scryfall Oracle text. A paraphrase could take a different parser
/// branch and pass while the real card stays unjournaled.
const DISTORTION_STRIKE_ORACLE: &str = "Target creature gets +1/+0 until end of turn and can't be blocked this turn.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";

/// Verbatim Scryfall Oracle text.
const GIANT_GROWTH_ORACLE: &str = "Target creature gets +3/+3 until end of turn.";

/// CR 603.7 + CR 702.88a: a Rebound spell cast from hand arms a delayed
/// triggered ability that fires at its controller's next upkeep. The install
/// goes through `triggers::install_delayed_trigger`, the single authority, so it
/// lands in the journal as an exact resolved command.
#[test]
fn rebound_journals_an_exact_resolved_delayed_trigger_install() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_vanilla(P0, 2, 2);
    // The MTGJSON keyword hint is what lets the keyword-only "Rebound (...)"
    // line be recognized as `Keyword::Rebound` rather than prose.
    let spell = scenario
        .add_spell_to_hand(P0, "Distortion Strike", true)
        .from_oracle_text_with_keywords(&["Rebound"], DISTORTION_STRIKE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Captured before the cast so the recorded command replays against the exact
    // predecessor state it was resolved from.
    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();
    assert!(
        pre_state.delayed_triggers.is_empty(),
        "the fixture must start with no delayed triggers, or the recorded install \
         position would not be the one this cast produced"
    );

    let outcome = runner.cast(spell).target_object(creature).resolve();
    let state = outcome.state();

    // REACH GUARD. Without this the journal assertion below could pass vacuously
    // on a cast whose Rebound never armed (e.g. a parser branch that dropped the
    // keyword, or a cast the engine treated as not-from-hand).
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "CR 702.88a: resolving Distortion Strike from hand must arm exactly one \
         delayed triggered ability"
    );
    assert!(
        matches!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: P0,
                ..
            }
        ),
        "CR 702.88a: the armed trigger fires at its controller's next upkeep, found {:?}",
        state.delayed_triggers[0].condition
    );

    // DISCRIMINATING ASSERTION: a raw `delayed_triggers.push` records nothing here.
    let installs: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::DelayedTriggerInstall(command) => Some(*command),
            _ => None,
        })
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "the delayed-trigger authority must journal exactly one resolved install"
    );

    let install = &installs[0];
    assert_eq!(
        install.trigger, state.delayed_triggers[0],
        "the journaled trigger is the trigger that was actually installed, with its \
         CR 603.7c bound ability intact"
    );
    assert_eq!(
        install.expected_installed_count, 0,
        "the recorded precondition is the install position observed at resolve time"
    );

    // REPLAY EXACTNESS: installing the recorded command into the captured
    // predecessor reproduces the same trigger, with no re-derivation of its
    // condition, controller, source, or bound targets.
    let mut replay = pre_state;
    engine::game::triggers::apply_resolved_delayed_trigger(&mut replay, install)
        .expect("the recorded install must replay against its captured predecessor");
    assert_eq!(
        replay.delayed_triggers, state.delayed_triggers,
        "replay installs the exact recorded delayed trigger"
    );

    // FAIL-CLOSED: the same command against a state that already has the trigger
    // is a diverged replay, and the applier must refuse rather than install a
    // duplicate CR 603.7 ability.
    assert_eq!(
        engine::game::triggers::apply_resolved_delayed_trigger(&mut replay, install),
        Err(
            ResolvedDelayedTriggerReplayInvariantError::InstalledCountPreconditionMismatch {
                expected: 0,
                found: 1,
            }
        ),
        "the applier must reject an install whose recorded position no longer matches \
         live state"
    );
    assert_eq!(
        replay.delayed_triggers.len(),
        1,
        "a rejected install must leave the collection untouched"
    );

    // A one-shot leaves the live queue once it fires, but its installation is
    // still a durable journal root. Replaying it after consumption must not
    // reuse its token/instance merely because the live queue is empty.
    let mut consumed = state.clone();
    consumed.delayed_triggers.clear();
    assert_eq!(
        engine::game::triggers::apply_resolved_delayed_trigger(&mut consumed, install),
        Err(
            ResolvedDelayedTriggerReplayInvariantError::DuplicateProvenanceToken {
                token: install.token,
            }
        ),
        "a consumed delayed trigger must retain its journaled install identity"
    );
    assert!(
        consumed.delayed_triggers.is_empty(),
        "a duplicate historical replay must not reinstall a consumed one-shot"
    );
}

/// CR 611.2a + CR 613.7b: a "gets +3/+3 until end of turn" spell creates a
/// transient continuous effect that draws an effect id and a layer timestamp.
/// `GameState::add_transient_continuous_effect` is the single authority, and it
/// journals both allocator draws so replay installs them instead of re-drawing.
#[test]
fn pump_spell_journals_an_exact_resolved_continuous_effect_install() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();
    assert!(
        pre_state.transient_continuous_effects.is_empty(),
        "the fixture must start with no continuous effects, or the recorded install \
         position would not be the one this cast produced"
    );

    let outcome = runner.cast(spell).target_object(creature).resolve();
    let state = outcome.state();

    // REACH GUARD. A pump that never resolved, or one the parser routed to a
    // different effect, would leave the creature at 2/2 and make the journal
    // assertion below vacuous.
    assert_eq!(
        (
            state.objects[&creature].power,
            state.objects[&creature].toughness
        ),
        (Some(5), Some(5)),
        "CR 613.1: the resolved +3/+3 must apply to the 2/2 through the layer system"
    );
    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "CR 611.2a: the resolved pump must install exactly one continuous effect"
    );

    // DISCRIMINATING ASSERTION: a raw `push_back` records nothing here.
    let installs: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ContinuousEffectInstall(command) => Some(*command),
            _ => None,
        })
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "the continuous-effect authority must journal exactly one resolved install"
    );

    let install = &installs[0];
    let live = &state.transient_continuous_effects[0];
    assert_eq!(
        &install.effect, live,
        "the journaled effect is the effect that was actually installed, with its \
         CR 611.2c affected set fixed"
    );
    assert_eq!(
        install.expected_installed_count, 0,
        "the recorded precondition is the install position observed at resolve time"
    );
    // CR 613.7b: the recorded timestamp is the one the effect actually received.
    // These two assertions are what make replay reproducible rather than
    // merely plausible.
    assert!(
        install.effect.timestamp < install.resulting_next_timestamp,
        "the recorded timestamp must lie below the high-water its draw left behind"
    );
    assert!(
        install.effect.id < install.resulting_next_continuous_effect_id,
        "the recorded effect id must lie below the high-water its draw left behind"
    );

    // REPLAY EXACTNESS: the effect, its id, and its layer timestamp are installed
    // verbatim, and both allocators are advanced past them so a later live draw
    // cannot hand the same values out again.
    let mut replay = pre_state;
    replay
        .apply_resolved_continuous_effect(install)
        .expect("the recorded install must replay against its captured predecessor");
    assert_eq!(
        replay.transient_continuous_effects, state.transient_continuous_effects,
        "replay installs the exact recorded continuous effect, id and timestamp included"
    );
    assert!(
        replay.next_continuous_effect_id >= install.resulting_next_continuous_effect_id,
        "replay must advance the effect-id allocator past the installed id"
    );
    assert!(
        replay.next_timestamp >= install.resulting_next_timestamp,
        "CR 613.7b: replay must advance the timestamp allocator past the installed timestamp"
    );

    // FAIL-CLOSED on the install position.
    assert_eq!(
        replay.apply_resolved_continuous_effect(install),
        Err(
            ResolvedContinuousEffectReplayInvariantError::InstalledCountPreconditionMismatch {
                expected: 0,
                found: 1,
            }
        ),
        "the applier must reject an install whose recorded position no longer matches \
         live state"
    );

    // FAIL-CLOSED on id uniqueness: a command whose position precondition DOES
    // match but whose id is already live must still be refused, because effects
    // are addressed by id and two live effects sharing one are indistinguishable
    // to every later lookup.
    let mut collides = install.clone();
    collides.expected_installed_count = 1;
    assert_eq!(
        replay.apply_resolved_continuous_effect(&collides),
        Err(ResolvedContinuousEffectReplayInvariantError::DuplicateEffectId(install.effect.id)),
        "the applier must reject an install that would duplicate a live effect id"
    );
    assert_eq!(
        replay.transient_continuous_effects.len(),
        1,
        "a rejected install must leave the collection untouched"
    );
}
