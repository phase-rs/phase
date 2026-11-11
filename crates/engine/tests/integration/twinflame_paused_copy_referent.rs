//! "Those tokens" across a paused copy-token batch, through the real cast and
//! activation pipelines.
//!
//! - CR 608.2c: the instructions are followed in written order, so "Exile those
//!   tokens" (Twinflame) and "Those tokens gain haste" (Saheeli, the Gifted's
//!   −7) name every token the producing instruction created — including the
//!   ones whose entry paused, or came after a batch that paused.
//! - CR 603.7a + CR 603.7c: the delayed exile is created during the resolution
//!   and acts on the members still on the battlefield when it resolves.
//! - Pauses reached: a counter-replacement ordering on a copy's "enters with"
//!   counters (CR 614.1c + CR 122.6 + CR 616.1e / CR 616.1f), an entry
//!   replacement ordering (Archelos + Imposing Sovereign, CR 616.1e), and an
//!   as-enters choice (Painter's Servant, CR 614.12a).
//! - Token multipliers are in the class (rulings, verbatim):
//!   - Twinflame: "if Twinflame creates multiple tokens copying a single creature
//!     due to a replacement effect (like the one Doubling Season creates),
//!     you'll exile each of them."
//!   - Chatterfang: anything the effect specifies, "Exile that token at end of
//!     combat" included, "applies to both the original tokens and the Squirrels."
//!
//! Every pause row also pins owner custody: the CopyToken owner stays on the
//! resolution stack across every non-final answer, and its one
//! `EffectResolved{CopyTokenOf}` follows the last `TokenCreated` of its batches.

use std::collections::HashSet;

use crate::support::shared_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, EffectKind, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::FrameKind;
use engine::types::zones::Zone;

// Verbatim Oracle text (MTGJSON); Saheeli, the Gifted is not in the
// integration fixture. "can be your commander" is not a game-time ability.
const SAHEELI: &str = "[+1]: Create a 1/1 colorless Servo artifact creature token.\n[+1]: The next spell you cast this turn has affinity for artifacts. (It costs {1} less to cast for each artifact you control as you cast it.)\n[\u{2212}7]: For each artifact you control, create a token that's a copy of it. Those tokens gain haste. Exile those tokens at the beginning of the next end step.";

const HARDENED_SCALES: &str = "Hardened Scales";
const BRANCHING_EVOLUTION: &str = "Branching Evolution";
const ARCHELOS: &str = "Archelos, Lagoon Mystic";
const IMPOSING_SOVEREIGN: &str = "Imposing Sovereign";
const GRUMGULLY: &str = "Grumgully, the Generous";
const PARALLEL_LIVES: &str = "Parallel Lives";
const CHATTERFANG: &str = "Chatterfang, Squirrel General";
const TRISKELION: &str = "Triskelion";
const BEARS: &str = "Grizzly Bears";
const PAINTERS_SERVANT: &str = "Painter's Servant";
const MEMNITE: &str = "Memnite";
const ARCBOUND_WORKER: &str = "Arcbound Worker";

/// Ordering answers: Hardened Scales first (so Branching Evolution doubles
/// the +1), Imposing Sovereign first, then Grumgully.
const PREFERENCE: &[&str] = &[HARDENED_SCALES, IMPOSING_SOVEREIGN, GRUMGULLY];

/// The answer bound: the most prompts a measured board raises is eight.
const MAX_PROMPTS: usize = 24;

/// Twinflame {1}{R}, plus Strive {2}{R} per target beyond the first.
fn twinflame_mana(spell: ObjectId, targets: usize) -> Vec<ManaUnit> {
    let mut pool = vec![
        ManaUnit::new(ManaType::Red, spell, false, vec![]),
        ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
    ];
    for _ in 1..targets {
        pool.push(ManaUnit::new(ManaType::Red, spell, false, vec![]));
        pool.push(ManaUnit::new(ManaType::Colorless, spell, false, vec![]));
        pool.push(ManaUnit::new(ManaType::Colorless, spell, false, vec![]));
    }
    pool
}

fn has_copy_token_frame(runner: &GameRunner) -> bool {
    runner
        .state()
        .resolution_stack
        .iter()
        .any(|frame| frame.kind() == FrameKind::CopyToken)
}

fn is_paused(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. } | WaitingFor::NamedChoice { .. }
    )
}

/// What one driven resolution observed, from the production entry point to
/// the settled priority window.
struct Driven {
    runner: GameRunner,
    /// The copied permanents (Twinflame's targets / Saheeli's artifacts).
    sources: Vec<ObjectId>,
    /// Battlefield before the producing action.
    pre: HashSet<ObjectId>,
    /// One event slice per action: the cast/activation, then each answer.
    actions: Vec<Vec<GameEvent>>,
    /// The candidate source names of every ordering prompt answered.
    orderings: Vec<Vec<String>>,
    /// Number of as-enters `NamedChoice` prompts answered.
    named_choices: usize,
    /// The chain's tracked set when the resolution first paused: the set the
    /// producing instruction published ("those tokens").
    producer_set_at_pause: Option<TrackedSetId>,
}

impl Driven {
    fn all_events(&self) -> impl Iterator<Item = &GameEvent> {
        self.actions.iter().flatten()
    }

    /// Every token the resolution created, in `TokenCreated` order.
    fn created_tokens(&self) -> Vec<ObjectId> {
        self.all_events()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect()
    }

    fn paused(&self) -> bool {
        self.actions.len() > 1
    }

    /// The tracked set the installed delayed exile binds, and its members.
    fn delayed_referent(&self) -> (TrackedSetId, Vec<ObjectId>) {
        let state = self.runner.state();
        let bound: Vec<TrackedSetId> = state
            .delayed_triggers
            .iter()
            .filter_map(|trigger| match &trigger.ability.effect {
                Effect::ChangeZoneAll {
                    target: TargetFilter::TrackedSet { id },
                    ..
                }
                | Effect::ChangeZone {
                    target: TargetFilter::TrackedSet { id },
                    ..
                } => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(
            bound.len(),
            1,
            "REACH-GUARD: exactly one delayed tracked-set exile must be installed, found {:?}",
            state
                .delayed_triggers
                .iter()
                .map(|trigger| format!("{:?}", trigger.ability.effect))
                .collect::<Vec<_>>()
        );
        let id = bound[0];
        let members = state
            .tracked_object_sets
            .get(&id)
            .cloned()
            .unwrap_or_default();
        (id, members)
    }
}

/// Answer every ordering and as-enters choice until the resolution settles,
/// asserting owner custody (the CopyToken frame survives every non-final
/// answer) and the completion boundary.
fn settle(mut driven: Driven) -> Driven {
    if is_paused(&driven.runner) {
        assert!(
            has_copy_token_frame(&driven.runner),
            "REACH-GUARD: the first pause must hold the CopyToken owner on the resolution stack, \
             frames {:?}",
            frame_kinds(&driven.runner)
        );
        driven.producer_set_at_pause = driven.runner.state().chain_tracked_set_id;
    }
    for _ in 0..MAX_PROMPTS {
        let action = match &driven.runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|candidate| candidate.source_name.clone())
                    .collect();
                let index = PREFERENCE
                    .iter()
                    .find_map(|preferred| names.iter().position(|name| name == preferred))
                    .unwrap_or(0);
                driven.orderings.push(names);
                GameAction::ChooseReplacement { index }
            }
            WaitingFor::NamedChoice { options, .. } => {
                driven.named_choices += 1;
                GameAction::ChooseOption {
                    choice: options[0].clone(),
                }
            }
            _ => break,
        };
        let result = driven.runner.act(action).expect("answer must be accepted");
        driven.actions.push(result.events);
        if is_paused(&driven.runner) {
            // CR 608.2c: the owner completes the instruction only after its
            // last batch commits.
            assert!(
                has_copy_token_frame(&driven.runner),
                "custody: the CopyToken owner must survive a non-final answer, frames {:?}",
                frame_kinds(&driven.runner)
            );
        }
    }
    let state = driven.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the resolution must settle at priority, got {:?}",
        state.waiting_for
    );
    assert!(
        state.resolution_stack.is_empty(),
        "the resolution stack must be empty once the instruction settles, frames {:?}",
        frame_kinds(&driven.runner)
    );
    assert!(state.stack.is_empty(), "the stack must be empty");

    // Owner custody: one terminal, after every token it created.
    let events: Vec<&GameEvent> = driven.all_events().collect();
    let resolved: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CopyTokenOf,
                    ..
                }
            )
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        resolved.len(),
        1,
        "custody: exactly one EffectResolved{{CopyTokenOf}}"
    );
    let last_created = events
        .iter()
        .rposition(|event| matches!(event, GameEvent::TokenCreated { .. }))
        .expect("REACH-GUARD: the resolution must create tokens");
    assert!(
        resolved[0] > last_created,
        "custody: EffectResolved{{CopyTokenOf}} must follow the last TokenCreated"
    );
    let final_action = driven
        .actions
        .last()
        .expect("at least the producing action");
    assert!(
        final_action.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::CopyTokenOf,
                ..
            }
        )),
        "custody: the owner completes in the action that settles the resolution"
    );
    driven
}

fn frame_kinds(runner: &GameRunner) -> Vec<FrameKind> {
    runner
        .state()
        .resolution_stack
        .iter()
        .map(|frame| frame.kind())
        .collect()
}

fn build_board(
    targets: &[&str],
    board: &[(PlayerId, &str)],
) -> Option<(GameScenario, Vec<ObjectId>)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Targets first, so the replacement permanents below do not prompt their
    // setup entries.
    let sources = targets
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    for (player, name) in board {
        scenario.add_real_card(*player, name, Zone::Battlefield, db);
    }
    Some((scenario, sources))
}

/// Cast Twinflame targeting `targets` over `board`, and settle it.
fn cast_twinflame(targets: &[&str], board: &[(PlayerId, &str)]) -> Option<Driven> {
    let db = shared_card_db()?;
    let (mut scenario, sources) = build_board(targets, board)?;
    let twinflame = scenario.add_real_card(P0, "Twinflame", Zone::Hand, db);
    scenario.with_mana_pool(P0, twinflame_mana(twinflame, targets.len()));
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let pre = runner.state().battlefield.iter().copied().collect();
    let cast = runner
        .cast(twinflame)
        .target_objects(&sources)
        .try_resolve()
        .expect("Twinflame must be castable");
    let actions = vec![cast.events().to_vec()];
    Some(settle(Driven {
        runner,
        sources,
        pre,
        actions,
        orderings: Vec::new(),
        named_choices: 0,
        producer_set_at_pause: None,
    }))
}

/// Activate Saheeli, the Gifted's −7 with `artifacts` over `board`, and settle it.
fn activate_saheeli(artifacts: &[&str], board: &[(PlayerId, &str)]) -> Option<Driven> {
    let (mut scenario, sources) = build_board(artifacts, &[])?;
    let saheeli = scenario
        .add_planeswalker_from_oracle(P0, "Saheeli, the Gifted", "Saheeli", 7, SAHEELI)
        .id();
    let db = shared_card_db()?;
    for (player, name) in board {
        scenario.add_real_card(*player, name, Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let pre = runner.state().battlefield.iter().copied().collect();
    let activation = runner
        .activate(saheeli, 2)
        .try_resolve()
        .expect("Saheeli's −7 must be activatable");
    let actions = vec![activation.events().to_vec()];
    Some(settle(Driven {
        runner,
        sources,
        pre,
        actions,
        orderings: Vec::new(),
        named_choices: 0,
        producer_set_at_pause: None,
    }))
}

/// The tokens now on the battlefield that the resolution created.
fn tokens_on_battlefield(driven: &Driven) -> Vec<ObjectId> {
    let state = driven.runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| !driven.pre.contains(id))
        .collect()
}

/// CR 603.7a + CR 603.7c: cross to the next end step and resolve its delayed
/// exile; every created token must be gone, every copied source must remain.
fn assert_end_step_exiles_every_token(mut driven: Driven) {
    let tokens = driven.created_tokens();
    assert_eq!(
        sorted(tokens_on_battlefield(&driven)),
        sorted(tokens.clone()),
        "REACH-GUARD: every created token is on the battlefield before the end step"
    );
    driven.runner.advance_to_combat();
    driven
        .runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    driven.runner.advance_to_end_step();
    driven.runner.advance_until_stack_empty();
    let state = driven.runner.state();
    let remaining: Vec<(ObjectId, String)> = tokens
        .iter()
        .filter(|id| state.battlefield.contains(id))
        .map(|id| (*id, state.objects[id].name.clone()))
        .collect();
    assert!(
        remaining.is_empty(),
        "\"those tokens\" must all be exiled at the next end step; still on the battlefield: \
         {remaining:?}"
    );
    for source in &driven.sources {
        assert_eq!(
            state.objects[source].zone,
            Zone::Battlefield,
            "the copied source {} must not be exiled",
            state.objects[source].name
        );
    }
}

/// The referent the delayed exile binds holds exactly the created tokens.
fn assert_referent_is_every_token(driven: &Driven) -> Vec<ObjectId> {
    let (id, members) = driven.delayed_referent();
    assert_ne!(
        id,
        TrackedSetId(0),
        "the delayed exile must bind a published set, not the parse-time placeholder"
    );
    assert_eq!(
        sorted(members.clone()),
        sorted(driven.created_tokens()),
        "the referent set {id:?} must hold every token the instruction created"
    );
    members
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

fn plus_one_counters(driven: &Driven, id: ObjectId) -> u32 {
    driven.runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn named(driven: &Driven, name: &str) -> Vec<ObjectId> {
    let state = driven.runner.state();
    driven
        .created_tokens()
        .into_iter()
        .filter(|id| state.objects[id].name == name)
        .collect()
}

fn assert_ordering_prompted(driven: &Driven, expected: &[&str]) {
    assert!(
        driven.paused(),
        "REACH-GUARD: the resolution must pause at least once"
    );
    let mut first: Vec<String> = driven.orderings.first().cloned().unwrap_or_default();
    first.sort();
    let mut want: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
    want.sort();
    assert_eq!(
        first, want,
        "REACH-GUARD: the first ordering prompt's candidates"
    );
}

fn counter_board() -> Vec<(PlayerId, &'static str)> {
    vec![(P0, HARDENED_SCALES), (P0, BRANCHING_EVOLUTION)]
}

fn entry_board() -> Vec<(PlayerId, &'static str)> {
    vec![(P0, ARCHELOS), (P1, IMPOSING_SOVEREIGN)]
}

/// R1 (P1): a single Triskelion copy's "enters with three +1/+1 counters"
/// pauses on the Hardened Scales / Branching Evolution ordering; the copy is
/// still one of "those tokens".
#[test]
fn r1_counter_prompt_pause_single_target_exiles_the_copy() {
    let Some(driven) = cast_twinflame(&[TRISKELION], &counter_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    let tokens = driven.created_tokens();
    assert_eq!(tokens.len(), 1);
    // CR 614.1c + CR 616.1f: Hardened Scales first gives (3 + 1) × 2.
    assert_eq!(plus_one_counters(&driven, tokens[0]), 8);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R2 (P2): an inline-committed Bears copy, then a Triskelion copy whose
/// counters pause; the referent keeps the pre-pause member first, once.
#[test]
fn r2_counter_prompt_after_inline_batch_keeps_creation_order() {
    let Some(driven) = cast_twinflame(&[BEARS, TRISKELION], &counter_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    let members = assert_referent_is_every_token(&driven);
    let bears = named(&driven, BEARS);
    let triskelion = named(&driven, TRISKELION);
    assert_eq!((bears.len(), triskelion.len()), (1, 1));
    assert_eq!(
        members,
        vec![bears[0], triskelion[0]],
        "the referent lists the pre-pause member first and once, then the paused batch's"
    );
    assert_end_step_exiles_every_token(driven);
}

/// R3 (P2b): the pause is in the first batch; the later batch is unpaused.
#[test]
fn r3_counter_prompt_in_first_batch_then_unpaused_batch() {
    let Some(driven) = cast_twinflame(&[TRISKELION, BEARS], &counter_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    assert_eq!(driven.created_tokens().len(), 2);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R4 (P9): two liminal Bears batches, each paused on the Archelos / Imposing
/// Sovereign entry ordering. `LastCreated` names both copies.
#[test]
fn r4_entry_ordering_pause_two_liminal_batches() {
    let Some(driven) = cast_twinflame(&[BEARS, BEARS], &entry_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    let tokens = driven.created_tokens();
    assert_eq!(tokens.len(), 2);
    assert!(
        driven.orderings.len() >= tokens.len(),
        "REACH-GUARD: at least one entry ordering per token, saw {}",
        driven.orderings.len()
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated must list every copy in creation order, once"
    );
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R5 (P4): the entry ordering pauses a liminal Bears batch and a non-liminal
/// Triskelion batch.
#[test]
fn r5_entry_ordering_pause_on_non_liminal_batch() {
    let Some(driven) = cast_twinflame(&[BEARS, TRISKELION], &entry_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    assert_eq!(driven.created_tokens().len(), 2);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R5 sibling (P8): the same board with the non-liminal batch first.
#[test]
fn r5_sibling_entry_ordering_non_liminal_batch_first() {
    let Some(driven) = cast_twinflame(&[TRISKELION, BEARS], &entry_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    let tokens = driven.created_tokens();
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated must list every copy in creation order, once"
    );
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R6 (P14): a three-way entry ordering (Grumgully, Archelos, Sovereign), then
/// a post-replacement counter prompt under Hardened Scales + Branching
/// Evolution. Counter totals are deliberately not asserted (see the plan's
/// fold-counter observation).
#[test]
fn r6_ordering_then_post_replacement_counter_prompt() {
    let board = [
        (P0, GRUMGULLY),
        (P0, ARCHELOS),
        (P1, IMPOSING_SOVEREIGN),
        (P0, HARDENED_SCALES),
        (P0, BRANCHING_EVOLUTION),
    ];
    let Some(driven) = cast_twinflame(&[BEARS, TRISKELION], &board) else {
        return;
    };
    assert_ordering_prompted(&driven, &[GRUMGULLY, ARCHELOS, IMPOSING_SOVEREIGN]);
    assert!(
        driven
            .orderings
            .iter()
            .any(|names| names.iter().any(|name| name == HARDENED_SCALES)),
        "REACH-GUARD: a counter ordering must follow the entry ordering"
    );
    assert_eq!(driven.created_tokens().len(), 2);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R7 (PS3): an entry ordering, then Painter's Servant's as-enters color
/// choice, whose post-replacement chain resolves at depth 0 and resets the
/// chain scope. The delayed exile must still bind the producer's set.
#[test]
fn r7_as_enters_choice_keeps_the_producer_set() {
    let Some(driven) = cast_twinflame(&[BEARS, PAINTERS_SERVANT], &entry_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    assert!(
        driven.named_choices >= 1,
        "REACH-GUARD: the Painter's Servant copy must raise its as-enters color choice"
    );
    let producer_set = driven
        .producer_set_at_pause
        .expect("REACH-GUARD: the producing instruction published its set before pausing");
    let (bound, _) = driven.delayed_referent();
    assert_eq!(
        bound, producer_set,
        "the delayed exile must bind the set the producing instruction published"
    );
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R11: Parallel Lives doubles a single Triskelion copy, and each copy's
/// counters pause. Twinflame ruling: "you'll exile each of them."
#[test]
fn r11_parallel_lives_doubled_copy_under_counter_prompt() {
    let board = [
        (P0, PARALLEL_LIVES),
        (P0, HARDENED_SCALES),
        (P0, BRANCHING_EVOLUTION),
    ];
    let Some(driven) = cast_twinflame(&[TRISKELION], &board) else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    assert_eq!(
        driven.created_tokens().len(),
        2,
        "REACH-GUARD: Parallel Lives must double the copy"
    );
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R12: Parallel Lives doubles two liminal Bears batches, every entry paused on
/// the Archelos / Imposing Sovereign ordering.
#[test]
fn r12_parallel_lives_doubled_liminal_batches_under_entry_ordering() {
    let board = [
        (P0, PARALLEL_LIVES),
        (P0, ARCHELOS),
        (P1, IMPOSING_SOVEREIGN),
    ];
    let Some(driven) = cast_twinflame(&[BEARS, BEARS], &board) else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.len(),
        4,
        "REACH-GUARD: Parallel Lives must double both copies"
    );
    let last_created = driven.runner.state().last_created_token_ids.clone();
    assert_eq!(
        last_created.len(),
        4,
        "LastCreated must list all four tokens once, got {last_created:?}"
    );
    assert_eq!(sorted(last_created), sorted(tokens));
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R13: Chatterfang adds a Squirrel to each copy batch, and the Triskelion
/// copy's counters pause. Chatterfang ruling: the creating effect's riders
/// apply "to both the original tokens and the Squirrels." Membership, not
/// order, is asserted.
#[test]
fn r13_chatterfang_squirrels_join_the_referent() {
    let board = [
        (P0, CHATTERFANG),
        (P0, HARDENED_SCALES),
        (P0, BRANCHING_EVOLUTION),
    ];
    let Some(driven) = cast_twinflame(&[TRISKELION, BEARS], &board) else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    assert_eq!(
        named(&driven, "Squirrel").len(),
        2,
        "REACH-GUARD: Chatterfang must add one Squirrel per copy batch"
    );
    assert_eq!(driven.created_tokens().len(), 4);
    let last_created = driven.runner.state().last_created_token_ids.clone();
    assert_eq!(sorted(last_created), sorted(driven.created_tokens()));
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R9 (P3): preservation control. Under Hardened Scales alone nothing pauses;
/// the referent the events published is both copies, exiled at end step.
#[test]
fn r9_unpaused_baseline_is_unchanged() {
    let Some(driven) = cast_twinflame(&[BEARS, TRISKELION], &[(P0, HARDENED_SCALES)]) else {
        return;
    };
    assert!(
        !driven.paused() && driven.orderings.is_empty(),
        "no ReplacementChoice may be raised under Hardened Scales alone"
    );
    let cast_tokens = driven.actions[0]
        .iter()
        .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
        .count();
    assert_eq!(cast_tokens, 2, "REACH-GUARD: TokenCreated ×2 in the cast");
    let triskelion = named(&driven, TRISKELION);
    // CR 614.1c + CR 122.6: three plus Hardened Scales' one.
    assert_eq!(plus_one_counters(&driven, triskelion[0]), 4);
    let members = assert_referent_is_every_token(&driven);
    assert_eq!(members, driven.created_tokens());
    assert_end_step_exiles_every_token(driven);
}

fn assert_every_token_has_haste(driven: &Driven) {
    let state = driven.runner.state();
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.len(),
        driven.sources.len(),
        "REACH-GUARD: one copy per artifact"
    );
    for id in &tokens {
        assert!(
            state.objects[id].has_keyword(&Keyword::Haste),
            "\"Those tokens gain haste\" must reach token {id:?}"
        );
    }
}

/// R8 (S3): Saheeli's −7 over two Memnites; each liminal entry pauses on the
/// Archelos / Imposing Sovereign ordering. "Those tokens gain haste" reads
/// `LastCreated`, so both copies gain haste and both are exiled.
#[test]
fn r8_saheeli_last_created_referent_across_liminal_resume() {
    let Some(driven) = activate_saheeli(&[MEMNITE, MEMNITE], &entry_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    assert_every_token_has_haste(&driven);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R8 sibling (S1): Saheeli over Arcbound Worker under Hardened Scales +
/// Branching Evolution; the modular counters pause. Green on main, because
/// the haste grant re-publishes from `LastCreated` after the owner completes.
#[test]
fn r8_sibling_saheeli_modular_counter_prompt() {
    let Some(driven) = activate_saheeli(&[ARCBOUND_WORKER], &counter_board()) else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    assert_every_token_has_haste(&driven);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R10 (S2): preservation control. Saheeli over Arcbound Worker and Memnite
/// with no replacements; nothing pauses.
#[test]
fn r10_saheeli_unpaused_control() {
    let Some(driven) = activate_saheeli(&[ARCBOUND_WORKER, MEMNITE], &[]) else {
        return;
    };
    assert!(
        !driven.paused(),
        "REACH-GUARD: no prompt without replacements"
    );
    assert_every_token_has_haste(&driven);
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}
