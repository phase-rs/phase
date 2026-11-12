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
//!   replacement ordering (Archelos + Imposing Sovereign, CR 616.1e), an
//!   as-enters choice (Painter's Servant, CR 614.12a), an "enter as a copy"
//!   choice (Quicksilver Gargantuan, CR 614.1c + CR 707.5), an optional token
//!   replacement declined (Jinnie Fay, CR 614.6), an ordering between token
//!   replacements (Parallel Lives + Halving Season, CR 616.1e), and an optional
//!   token replacement accepted, whose substitute creation's tokens join the
//!   referent (Jinnie Fay's Cats and Dogs, Moonlit Meditation's and Esix's
//!   copies; CR 614.6 and Jinnie Fay's ruling, verbatim: "Anything else
//!   specified in the effect creating the tokens (such as tapped, attacking,
//!   “That token gains haste,” or “Exile that token at end of combat”) still
//!   applies.").
//! - A batch a replacement makes zero creates nothing, so it republishes nothing
//!   (Kiki-Jiki's "Sacrifice it").
//! - Token multipliers are in the class (rulings, verbatim):
//!   - Twinflame: "if Twinflame creates multiple tokens copying a single creature
//!     due to a replacement effect (like the one Doubling Season creates),
//!     you'll exile each of them."
//!   - Chatterfang: anything the effect specifies, "Exile that token at end of
//!     combat" included, "applies to both the original tokens and the Squirrels."
//!
//! Every pause row also pins owner custody: the CopyToken owner stays on the
//! resolution stack across every non-final answer, and its one
//! `EffectResolved{CopyTokenOf}` follows the last `TokenCreated` of its batches,
//! and the owner completes in the answer that settles the resolution, with no
//! priority window and no phase change before it (CR 117.2e).

use std::collections::HashSet;

use crate::support::shared_card_db;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, EffectKind, TargetFilter, TargetRef};
use engine::types::actions::{GameAction, ReplacementAutoChoice};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    OfferedBatchCustody, PendingCopyTokenResolution, PersistedGameState,
    PersistedRestoreFinalization, ReplacementCandidateSummary, ReplacementChoiceKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::{FrameKind, ResolutionFrame};
use engine::types::zones::Zone;

// Verbatim Oracle text (client card-data / MTGJSON); Saheeli, the Gifted is not in the
// integration fixture. The last line is a deck-construction permission (CR 903.3a), which the
// parser reads as no game-time ability.
const SAHEELI: &str = "[+1]: Create a 1/1 colorless Servo artifact creature token.\n[+1]: The next spell you cast this turn has affinity for artifacts. (It costs {1} less to cast for each artifact you control as you cast it.)\n[\u{2212}7]: For each artifact you control, create a token that's a copy of it. Those tokens gain haste. Exile those tokens at the beginning of the next end step.\nSaheeli, the Gifted can be your commander.";

const HARDENED_SCALES: &str = "Hardened Scales";
const BRANCHING_EVOLUTION: &str = "Branching Evolution";
const ARCHELOS: &str = "Archelos, Lagoon Mystic";
const IMPOSING_SOVEREIGN: &str = "Imposing Sovereign";
const GRUMGULLY: &str = "Grumgully, the Generous";
const PARALLEL_LIVES: &str = "Parallel Lives";
const DOUBLING_SEASON: &str = "Doubling Season";
const CHATTERFANG: &str = "Chatterfang, Squirrel General";
const TRISKELION: &str = "Triskelion";
const BEARS: &str = "Grizzly Bears";
const PAINTERS_SERVANT: &str = "Painter's Servant";
const MEMNITE: &str = "Memnite";
const ARCBOUND_WORKER: &str = "Arcbound Worker";
const QUICKSILVER_GARGANTUAN: &str = "Quicksilver Gargantuan";
const JINNIE: &str = "Jinnie Fay, Jetmir's Second";
const CLONE_LEGION: &str = "Clone Legion";
const HALVING_SEASON: &str = "Halving Season";
const KIKI_JIKI: &str = "Kiki-Jiki, Mirror Breaker";
const RAISE_THE_ALARM: &str = "Raise the Alarm";
const MOONLIT_MEDITATION: &str = "Moonlit Meditation";
const ESIX: &str = "Esix, Fractal Bloom";
const METALLIC_MIMIC: &str = "Metallic Mimic";

/// Targets set up before every other permanent: a creature that "may enter as
/// a copy of any creature on the battlefield" would otherwise prompt its own
/// setup entry.
const SETUP_FIRST: &[&str] = &[QUICKSILVER_GARGANTUAN];

/// Ordering answers: Hardened Scales first (so Branching Evolution doubles
/// the +1), Imposing Sovereign first, then Grumgully.
const PREFERENCE: &[&str] = &[HARDENED_SCALES, IMPOSING_SOVEREIGN, GRUMGULLY];

/// The answer bound: the most prompts a measured board raises is fifteen (R12c).
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
        WaitingFor::ReplacementChoice { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::CopyTargetChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
    )
}

/// The parked CopyToken owner, or `None` when no owner is on the resolution stack.
fn copy_token_owner(runner: &GameRunner) -> Option<&PendingCopyTokenResolution> {
    runner
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            ResolutionFrame::CopyToken(owner) => Some(owner),
            _ => None,
        })
}

/// The tokens the parked CopyToken owner already lists ("those tokens" so far),
/// or `None` when no owner is on the resolution stack.
fn copy_token_owner_ids(runner: &GameRunner) -> Option<Vec<ObjectId>> {
    copy_token_owner(runner).map(|owner| owner.created_ids.clone())
}

/// What one driven resolution observed, from the production entry point to
/// the settled priority window.
struct Driven {
    runner: GameRunner,
    /// The spell or activated permanent whose copy instruction the row judges:
    /// its owner's `EffectResolved{CopyTokenOf}` names it. A substitute's own
    /// copy owner (Moonlit Meditation, Esix) names its replacement's host.
    producer: ObjectId,
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

/// The default answer to one prompt: an ordering by [`PREFERENCE`], an
/// as-enters choice's first option. `None` for any other prompt.
fn default_answer(waiting: &WaitingFor, driven: &mut Driven) -> Option<GameAction> {
    match waiting {
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
            Some(GameAction::ChooseReplacement { index })
        }
        WaitingFor::NamedChoice { options, .. } => {
            driven.named_choices += 1;
            Some(GameAction::ChooseOption {
                choice: options[0].clone(),
            })
        }
        _ => None,
    }
}

/// Answer every ordering and as-enters choice until the resolution settles,
/// asserting owner custody (the CopyToken frame survives every non-final
/// answer) and the completion boundary.
fn settle(driven: Driven) -> Driven {
    settle_with(driven, default_answer)
}

/// [`settle`], answering each prompt with `answer` (`None` stops answering).
fn settle_with(
    mut driven: Driven,
    mut answer: impl FnMut(&WaitingFor, &mut Driven) -> Option<GameAction>,
) -> Driven {
    let phase = driven.runner.state().phase;
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
        let waiting = driven.runner.state().waiting_for.clone();
        let Some(action) = answer(&waiting, &mut driven) else {
            break;
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
    assert_eq!(
        state.phase, phase,
        "CR 500.2 + CR 117.2e: the step cannot end while the resolution is in progress"
    );

    // Owner custody: the producer's one terminal, after every token it created.
    let producer = driven.producer;
    let is_producer_terminal = |event: &GameEvent| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::CopyTokenOf,
                source_id,
                ..
            } if *source_id == producer
        )
    };
    let events: Vec<&GameEvent> = driven.all_events().collect();
    let resolved: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| is_producer_terminal(event))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        resolved.len(),
        1,
        "custody: exactly one EffectResolved{{CopyTokenOf}} for the producer"
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
        final_action.iter().any(is_producer_terminal),
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

/// The engine's own save / undo / P2P restore pipeline (`PersistedGameState`'s trusted
/// envelope), with the deferred production reload (as
/// `grist_zone_functioning.rs::cast_prohibition_restore`) — not a bare
/// `serde_json::from_str::<GameState>`.
fn save_and_restore(runner: &GameRunner) -> GameRunner {
    let db = shared_card_db().expect("REACH-GUARD: a restore needs the integration CardDB");
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the paused state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("the paused state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the paused state is admissible")
        .finalize_after_rehydration(|state| {
            engine::game::rehydrate_game_from_card_db_with_finalization(
                state,
                db,
                engine::game::CardDbRehydrationFinalization::Defer,
            );
            Ok(())
        })
        .expect("the restored state is publishable");
    GameRunner::from_state(state)
}

fn build_board(
    targets: &[&str],
    board: &[(PlayerId, &str)],
) -> Option<(GameScenario, Vec<ObjectId>)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Targets first, so the replacement permanents below do not prompt their
    // setup entries; a copy-on-entry target first of all, while there is no
    // creature for its as-enters copy to choose. Returned in target order.
    let mut placement: Vec<usize> = (0..targets.len()).collect();
    placement.sort_by_key(|&index| !SETUP_FIRST.contains(&targets[index]));
    let mut placed: Vec<(usize, ObjectId)> = placement
        .into_iter()
        .map(|index| {
            let id = scenario.add_real_card(P0, targets[index], Zone::Battlefield, db);
            (index, id)
        })
        .collect();
    placed.sort_by_key(|(index, _)| *index);
    let sources = placed.into_iter().map(|(_, id)| id).collect();
    for (player, name) in board {
        scenario.add_real_card(*player, name, Zone::Battlefield, db);
    }
    Some((scenario, sources))
}

/// Cast Twinflame targeting `targets` over `board`, and settle it.
fn cast_twinflame(targets: &[&str], board: &[(PlayerId, &str)]) -> Option<Driven> {
    cast_twinflame_with(targets, board, default_answer)
}

/// [`cast_twinflame`], answering each prompt with `answer`.
fn cast_twinflame_with(
    targets: &[&str],
    board: &[(PlayerId, &str)],
    answer: impl FnMut(&WaitingFor, &mut Driven) -> Option<GameAction>,
) -> Option<Driven> {
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
    Some(settle_with(
        Driven {
            runner,
            producer: twinflame,
            sources,
            pre,
            actions,
            orderings: Vec::new(),
            named_choices: 0,
            producer_set_at_pause: None,
        },
        answer,
    ))
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
        producer: saheeli,
        sources,
        pre,
        actions,
        orderings: Vec::new(),
        named_choices: 0,
        producer_set_at_pause: None,
    }))
}

/// Cast Clone Legion targeting P0 over `board` (P0's creatures are two Grizzly
/// Bears), answering each prompt with `answer`, and settle it.
fn cast_clone_legion_with(
    board: &[(PlayerId, &str)],
    answer: impl FnMut(&WaitingFor, &mut Driven) -> Option<GameAction>,
) -> Option<Driven> {
    let db = shared_card_db()?;
    let (mut scenario, sources) = build_board(&[BEARS, BEARS], board)?;
    let legion = scenario.add_real_card(P0, CLONE_LEGION, Zone::Hand, db);
    // Clone Legion {7}{U}{U}.
    let mut pool = vec![
        ManaUnit::new(ManaType::Blue, legion, false, vec![]),
        ManaUnit::new(ManaType::Blue, legion, false, vec![]),
    ];
    pool.extend((0..7).map(|_| ManaUnit::new(ManaType::Colorless, legion, false, vec![])));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let pre = runner.state().battlefield.iter().copied().collect();
    let cast = runner
        .cast(legion)
        .target_player(P0)
        .try_resolve()
        .expect("Clone Legion must be castable");
    let actions = vec![cast.events().to_vec()];
    Some(settle_with(
        Driven {
            runner,
            producer: legion,
            sources,
            pre,
            actions,
            orderings: Vec::new(),
            named_choices: 0,
            producer_set_at_pause: None,
        },
        answer,
    ))
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

/// How R7 carries its pauses: in memory, or through the save/restore wire.
#[derive(Clone, Copy)]
enum PauseCarry {
    Live,
    SavedAndRestored,
}

/// R7 (PS3): an entry ordering, then Painter's Servant's as-enters color
/// choice, whose post-replacement chain resolves at depth 0 and resets the
/// chain scope. The delayed exile must still bind the producer's set.
#[test]
fn r7_as_enters_choice_keeps_the_producer_set() {
    r7_as_enters_choice(PauseCarry::Live);
}

/// R7 run through the engine's save/restore pipeline at every pause while the
/// parked owner holds its referent set (CR 608.2c + CR 603.7a).
#[test]
fn r7_as_enters_choice_keeps_the_producer_set_across_save_and_restore() {
    r7_as_enters_choice(PauseCarry::SavedAndRestored);
}

fn r7_as_enters_choice(carry: PauseCarry) {
    let mut restores = 0;
    let mut named_restores = 0;
    let driven = cast_twinflame_with(
        &[BEARS, PAINTERS_SERVANT],
        &entry_board(),
        |waiting, driven| {
            match carry {
                PauseCarry::Live => {}
                PauseCarry::SavedAndRestored if is_paused(&driven.runner) => {
                    let set = driven
                        .producer_set_at_pause
                        .expect("REACH-GUARD: the producer published its set before pausing");
                    let owner = copy_token_owner(&driven.runner)
                        .cloned()
                        .expect("REACH-GUARD: the CopyToken owner is parked");
                    assert_eq!(
                        owner.referent_set,
                        Some(set),
                        "REACH-GUARD: the owner holds the populated referent while the game is \
                         saved"
                    );
                    let members = driven.runner.state().tracked_object_sets.get(&set).cloned();
                    let next_set_id = driven.runner.state().next_tracked_set_id;
                    if matches!(waiting, WaitingFor::NamedChoice { .. }) {
                        assert_ne!(
                            driven.runner.state().chain_tracked_set_id,
                            Some(set),
                            "REACH-GUARD: at the as-enters choice the chain scope no longer \
                             names the producer set (the depth-0 entry chain reset it), so the \
                             owner's `referent_set` is its only carrier across the wire"
                        );
                        named_restores += 1;
                    }
                    driven.runner = save_and_restore(&driven.runner);
                    restores += 1;
                    assert_eq!(
                        copy_token_owner(&driven.runner).cloned(),
                        Some(owner),
                        "the owner, its `referent_set` included, crosses the wire unchanged"
                    );
                    let state = driven.runner.state();
                    assert_eq!(
                        state.tracked_object_sets.get(&set).cloned(),
                        members,
                        "the producer set's membership crosses the wire unchanged"
                    );
                    assert_eq!(
                        state.next_tracked_set_id, next_set_id,
                        "the tracked-set allocator crosses the wire unchanged"
                    );
                    assert_eq!(
                        state.waiting_for, *waiting,
                        "the restored game is at the same prompt"
                    );
                }
                PauseCarry::SavedAndRestored => {}
            }
            default_answer(waiting, driven)
        },
    );
    let Some(driven) = driven else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    assert!(
        driven.named_choices >= 1,
        "REACH-GUARD: the Painter's Servant copy must raise its as-enters color choice"
    );
    match carry {
        PauseCarry::Live => {}
        PauseCarry::SavedAndRestored => assert!(
            restores >= 2 && named_restores == 1,
            "REACH-GUARD: the game was saved and restored at an entry ordering and at the \
             as-enters choice, saw {restores} restores, {named_restores} at the choice"
        ),
    }
    let producer_set = driven
        .producer_set_at_pause
        .expect("REACH-GUARD: the producing instruction published its set before pausing");
    let (bound, _) = driven.delayed_referent();
    assert_eq!(
        bound, producer_set,
        "the delayed exile must bind the set the producing instruction published"
    );
    let members = assert_referent_is_every_token(&driven);
    assert_eq!(
        members,
        driven.created_tokens(),
        "the producer's set lists the copies in creation order"
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        driven.created_tokens(),
        "LastCreated must list both copies in creation order, once"
    );
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

/// R12c: Parallel Lives and Doubling Season quadruple three liminal Bears
/// batches (twelve copies), and every entry pauses on the Archelos / Imposing
/// Sovereign ordering, so each batch resumes repeatedly with a growing ledger.
/// Twinflame ruling: "you'll exile each of them." At every prompt the owner's
/// list holds each copy so far exactly once, in creation order.
#[test]
fn r12c_doubled_twice_liminal_batches_resume_repeatedly_without_duplicates() {
    let board = [
        (P0, PARALLEL_LIVES),
        (P0, DOUBLING_SEASON),
        (P0, ARCHELOS),
        (P1, IMPOSING_SOVEREIGN),
    ];
    // The owner's list at every prompt answered while it is parked.
    let mut owner_snapshots: Vec<Vec<ObjectId>> = Vec::new();
    let driven = cast_twinflame_with(&[BEARS, BEARS, BEARS], &board, |waiting, driven| {
        if let Some(ids) = copy_token_owner_ids(&driven.runner) {
            owner_snapshots.push(ids);
        }
        default_answer(waiting, driven)
    });
    let Some(driven) = driven else {
        return;
    };
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.len(),
        12,
        "REACH-GUARD: Parallel Lives and Doubling Season must each double all three copies"
    );
    let entry_orderings = driven
        .orderings
        .iter()
        .filter(|names| {
            let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
            names.sort_unstable();
            names == [ARCHELOS, IMPOSING_SOVEREIGN]
        })
        .count();
    assert!(
        entry_orderings >= tokens.len(),
        "REACH-GUARD: at least one entry ordering per token, saw {entry_orderings}"
    );
    assert!(
        owner_snapshots
            .iter()
            .any(|ids| ids.len() > 4 && ids.len() % 4 != 0),
        "REACH-GUARD: a later batch must resume while the owner lists an earlier batch and \
         part of its own, saw {owner_snapshots:?}"
    );
    for snapshot in &owner_snapshots {
        assert_eq!(
            Some(snapshot.as_slice()),
            tokens.get(..snapshot.len()),
            "every resume leaves the owner listing each copy so far once, in creation order"
        );
    }
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated must list all twelve copies in creation order, once"
    );
    let members = assert_referent_is_every_token(&driven);
    assert_eq!(
        members.iter().collect::<HashSet<_>>().len(),
        members.len(),
        "the referent lists each copy once"
    );
    assert_eq!(
        members, tokens,
        "the referent lists the copies in creation order"
    );
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

/// R14: the Quicksilver Gargantuan copy "may enter as a copy of any creature on
/// the battlefield"; it copies the original Triskelion, so Triskelion's
/// "enters with three +1/+1 counters" applies to it (CR 707.5) and pauses on a
/// second Hardened Scales / Branching Evolution ordering. That answer commits
/// the second batch's entry (`EmitCommittedCopyTokenEntry`) while the owner
/// already lists the first batch's Triskelion copy, and both stay in "those
/// tokens".
#[test]
fn r14_copy_target_entry_counter_prompt_keeps_the_earlier_batch() {
    let mut copy_target_choices = 0;
    let mut optional_copies = 0;
    // The owner's list and the batch ledger as the counter ordering that
    // follows the copy choice is answered.
    let mut at_second_counter_ordering: Option<(Vec<ObjectId>, Vec<ObjectId>)> = None;
    let driven = cast_twinflame_with(
        &[TRISKELION, QUICKSILVER_GARGANTUAN],
        &counter_board(),
        |waiting, driven| match waiting {
            WaitingFor::ReplacementChoice {
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            } => {
                optional_copies += 1;
                // Index 0 accepts the "you may" copy.
                Some(GameAction::ChooseReplacement { index: 0 })
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                copy_target_choices += 1;
                let triskelion = driven.sources[0];
                assert!(
                    valid_targets.contains(&triskelion),
                    "REACH-GUARD: the original Triskelion must be a legal copy choice, got \
                     {valid_targets:?}"
                );
                Some(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(triskelion)),
                })
            }
            WaitingFor::ReplacementChoice { .. } if copy_target_choices == 1 => {
                at_second_counter_ordering.get_or_insert_with(|| {
                    (
                        copy_token_owner_ids(&driven.runner).unwrap_or_default(),
                        driven.runner.state().last_created_token_ids.clone(),
                    )
                });
                default_answer(waiting, driven)
            }
            _ => default_answer(waiting, driven),
        },
    );
    let Some(driven) = driven else {
        return;
    };
    assert_ordering_prompted(&driven, &[HARDENED_SCALES, BRANCHING_EVOLUTION]);
    assert_eq!(
        (optional_copies, copy_target_choices),
        (1, 1),
        "REACH-GUARD: the Gargantuan copy's optional copy and its copy choice are each raised once"
    );
    let tokens = driven.created_tokens();
    assert_eq!(tokens.len(), 2, "REACH-GUARD: one copy per target");
    let (earlier, later) = (tokens[0], tokens[1]);
    let state = driven.runner.state();
    assert_eq!(
        (
            state.objects[&earlier].name.as_str(),
            state.objects[&later].name.as_str()
        ),
        (TRISKELION, TRISKELION),
        "REACH-GUARD: the Gargantuan copy became a copy of Triskelion"
    );
    // CR 614.1c + CR 616.1f: Hardened Scales first gives (3 + 1) × 2 on each.
    assert_eq!(
        (
            plus_one_counters(&driven, earlier),
            plus_one_counters(&driven, later)
        ),
        (8, 8),
        "REACH-GUARD: both copies' counter entries were ordered"
    );
    let (owner, ledger) = at_second_counter_ordering
        .expect("REACH-GUARD: a counter ordering must follow the copy choice");
    assert_eq!(
        (owner, ledger),
        (vec![earlier], vec![later]),
        "REACH-GUARD: the later batch's entry commits while the owner lists only the earlier \
         batch's copy and the batch ledger only the later one"
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated must list both copies in creation order, once"
    );
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}

/// R12b: R12's board, with the second batch's entry orderings answered by
/// remembering Imposing Sovereign before Archelos (CR 616.1). The second batch's
/// second token then enters without a prompt, and the liminal batch terminal
/// runs in that same action while the owner lists the first batch's tokens.
#[test]
fn r12b_remembered_ordering_enters_the_last_liminal_token_inline() {
    let board = [
        (P0, PARALLEL_LIVES),
        (P0, ARCHELOS),
        (P1, IMPOSING_SOVEREIGN),
    ];
    let mut remembered = 0;
    // The owner's list as the first remembered ordering is answered.
    let mut owner_at_first_remember: Option<Vec<ObjectId>> = None;
    let driven = cast_twinflame_with(&[BEARS, BEARS], &board, |waiting, driven| match waiting {
        // Batch 2: the owner already lists both of batch 1's tokens.
        WaitingFor::ReplacementChoice { candidates, .. }
            if copy_token_owner_ids(&driven.runner).is_some_and(|ids| ids.len() == 2) =>
        {
            remembered += 1;
            owner_at_first_remember
                .get_or_insert_with(|| copy_token_owner_ids(&driven.runner).unwrap_or_default());
            let position = |name: &str| {
                candidates
                    .iter()
                    .position(|candidate| candidate.source_name == name)
                    .expect("REACH-GUARD: both entry replacements are candidates")
            };
            driven.orderings.push(
                candidates
                    .iter()
                    .map(|candidate| candidate.source_name.clone())
                    .collect(),
            );
            Some(GameAction::ChooseReplacementAndRemember {
                choice: ReplacementAutoChoice::Order {
                    order: vec![position(IMPOSING_SOVEREIGN), position(ARCHELOS)],
                },
            })
        }
        _ => default_answer(waiting, driven),
    });
    let Some(driven) = driven else {
        return;
    };
    assert_ordering_prompted(&driven, &[ARCHELOS, IMPOSING_SOVEREIGN]);
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.len(),
        4,
        "REACH-GUARD: Parallel Lives must double both copies"
    );
    assert!(
        remembered >= 1,
        "REACH-GUARD: batch 2 must raise an entry ordering"
    );
    assert_eq!(
        owner_at_first_remember,
        Some(tokens[..2].to_vec()),
        "REACH-GUARD: batch 2's ordering is answered while the owner lists batch 1's tokens"
    );
    let settling: Vec<ObjectId> = driven
        .actions
        .last()
        .expect("at least the producing action")
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        settling,
        tokens[2..].to_vec(),
        "REACH-GUARD: the remembered ordering enters batch 2's second token inline, in the \
         action that settles the resolution"
    );
    let last_created = driven.runner.state().last_created_token_ids.clone();
    assert_eq!(
        last_created, tokens,
        "LastCreated must list all four tokens in creation order, once"
    );
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

/// R15: Twinflame on two Grizzly Bears under Jinnie Fay, Jetmir's Second, whose
/// "you may instead" is declined at both copies' token prompts. CR 614.6: nothing
/// is replaced, so each target's original copy is created, and each copy batch
/// is handed to the CopyToken owner once (CR 608.2c).
#[test]
fn r15_declined_optional_token_replacements_hand_each_copy_over_once() {
    // At each Jinnie prompt: the owner's list and the tokens created so far.
    let mut at_prompt: Vec<(Option<Vec<ObjectId>>, Vec<ObjectId>)> = Vec::new();
    let driven = cast_twinflame_with(&[BEARS, BEARS], &[(P0, JINNIE)], |waiting, driven| {
        let WaitingFor::ReplacementChoice { candidates, .. } = waiting else {
            return None;
        };
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.source_name == JINNIE),
            "REACH-GUARD: every prompt is Jinnie Fay's optional token replacement, got \
             {candidates:?}"
        );
        let decline = candidates
            .iter()
            .position(|candidate| candidate.description == "Decline")
            .expect("REACH-GUARD: Jinnie Fay's prompt offers a Decline answer");
        driven.orderings.push(
            candidates
                .iter()
                .map(|candidate| candidate.source_name.clone())
                .collect(),
        );
        at_prompt.push((
            copy_token_owner_ids(&driven.runner),
            driven.created_tokens(),
        ));
        Some(GameAction::ChooseReplacement { index: decline })
    });
    let Some(driven) = driven else {
        return;
    };
    assert_eq!(
        at_prompt.len(),
        2,
        "REACH-GUARD: one Jinnie Fay prompt per copy batch, saw {:?}",
        driven.orderings
    );
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.iter().collect::<HashSet<_>>().len(),
        2,
        "declining both prompts creates both original copies, got {tokens:?}"
    );
    assert_eq!(
        tokens.len(),
        2,
        "REACH-GUARD: no substitute Cat or Dog is created on decline"
    );
    let state = driven.runner.state();
    for id in &tokens {
        assert_eq!(
            state.objects[id].name, BEARS,
            "REACH-GUARD: each token is the original Bears copy"
        );
        assert!(
            state.objects[id].has_keyword(&Keyword::Haste),
            "REACH-GUARD: Twinflame's copy has haste"
        );
    }
    let (owner_at_second, created_at_second) = at_prompt[1].clone();
    assert_eq!(
        created_at_second.len(),
        1,
        "REACH-GUARD: the second prompt follows the first batch's copy"
    );
    assert_eq!(
        owner_at_second,
        Some(created_at_second),
        "at the second prompt the owner lists the first copy once, in TokenCreated order"
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated lists each copy once, in TokenCreated order"
    );
    let members = assert_referent_is_every_token(&driven);
    assert_eq!(
        members, tokens,
        "the referent lists the copies in TokenCreated order"
    );
    assert_end_step_exiles_every_token(driven);
}

/// R16: Clone Legion copies P0's two Grizzly Bears under P0's Parallel Lives
/// and P1's Halving Season. CR 616.1e: P0 orders the two at each copy's event.
/// Parallel Lives first gives 1 × 2 ÷ 2 = one copy; Halving Season first gives
/// ⌊1 ÷ 2⌋ = 0, and twice nothing is nothing. "LastCreated" lists the one copy
/// once: the zero-token batch republishes nothing.
#[test]
fn r16_ordered_token_replacements_one_copy_then_none() {
    // The owner's list at each ordering prompt.
    let mut owner_at_prompt: Vec<Option<Vec<ObjectId>>> = Vec::new();
    let board = [(P0, PARALLEL_LIVES), (P1, HALVING_SEASON)];
    let driven = cast_clone_legion_with(&board, |waiting, driven| {
        let WaitingFor::ReplacementChoice { candidates, .. } = waiting else {
            return None;
        };
        let mut names: Vec<String> = candidates
            .iter()
            .map(|candidate| candidate.source_name.clone())
            .collect();
        let first = match owner_at_prompt.len() {
            0 => PARALLEL_LIVES,
            1 => HALVING_SEASON,
            _ => return None,
        };
        let index = names
            .iter()
            .position(|name| name == first)
            .expect("REACH-GUARD: both token replacements are candidates");
        names.sort();
        driven.orderings.push(names);
        owner_at_prompt.push(copy_token_owner_ids(&driven.runner));
        Some(GameAction::ChooseReplacement { index })
    });
    let Some(driven) = driven else {
        return;
    };
    let want: Vec<String> = [HALVING_SEASON, PARALLEL_LIVES]
        .iter()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(
        driven.orderings,
        vec![want.clone(), want],
        "REACH-GUARD: each copy's event is ordered between Parallel Lives and Halving Season"
    );
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.len(),
        1,
        "REACH-GUARD: the first ordering yields one copy and the second none"
    );
    assert_eq!(
        driven.runner.state().objects[&tokens[0]].name,
        BEARS,
        "REACH-GUARD: the one token is a Bears copy"
    );
    assert_eq!(
        owner_at_prompt[1],
        Some(tokens.clone()),
        "at the second ordering the owner lists the first copy once"
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated lists the one copy once; the zero-token batch republishes nothing"
    );
}

/// A Kiki-Jiki, Mirror Breaker board: P0 controls Kiki-Jiki and `target`, and
/// `board` adds the rest. `moonlit` attaches P0's Moonlit Meditation to the
/// target; `raise_the_alarm` casts Raise the Alarm ({1}{W}) before the
/// activation, so `LastCreated` names its Soldiers when the copy begins.
struct KikiJikiBoard {
    target: &'static str,
    board: Vec<(PlayerId, &'static str)>,
    moonlit: bool,
    raise_the_alarm: bool,
}

/// One prompt a Kiki-Jiki run answered: its kind, its candidates' source
/// names, and the CopyToken frames on the resolution stack when it was raised.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KikiJikiPrompt {
    kind: &'static str,
    candidates: Vec<String>,
    copy_token_frames: usize,
}

/// What a Kiki-Jiki run shows: the Soldiers, the activation's events (every
/// answer's included), every prompt answered, the activation's tokens' names
/// and LastCreated once the activation resolved, and the end step's
/// resolution events.
struct KikiJikiRun {
    runner: GameRunner,
    kiki: ObjectId,
    soldiers: Vec<ObjectId>,
    activation: Vec<GameEvent>,
    prompts: Vec<KikiJikiPrompt>,
    token_names: Vec<String>,
    last_created_after_activation: Vec<ObjectId>,
    end_step: Vec<GameEvent>,
}

impl KikiJikiRun {
    /// Every token the activation created, in `TokenCreated` order.
    fn created_tokens(&self) -> Vec<ObjectId> {
        self.activation
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect()
    }
}

fn copy_token_frame_count(runner: &GameRunner) -> usize {
    frame_kinds(runner)
        .into_iter()
        .filter(|kind| *kind == FrameKind::CopyToken)
        .count()
}

/// R17's driver: Raise the Alarm, then Kiki-Jiki on the Bears; `halving` adds
/// P1's Halving Season. No prompt is answered.
fn run_kiki_jiki(halving: bool) -> Option<KikiJikiRun> {
    let board = if halving {
        vec![(P1, HALVING_SEASON)]
    } else {
        Vec::new()
    };
    run_kiki_jiki_with(
        KikiJikiBoard {
            target: BEARS,
            board,
            moonlit: false,
            raise_the_alarm: true,
        },
        |_, _, _| None,
    )
}

/// Activate Kiki-Jiki on `spec.target`, answer each prompt with `answer` (given
/// the prompt, the runner and the prompts answered so far; `None` stops
/// answering), and resolve the next end step's triggers.
fn run_kiki_jiki_with(
    spec: KikiJikiBoard,
    answer: impl FnMut(&WaitingFor, &GameRunner, &[KikiJikiPrompt]) -> Option<GameAction>,
) -> Option<KikiJikiRun> {
    run_kiki_jiki_with_pause_carry(spec, PauseCarry::Live, answer)
}

/// Carry each answered prompt either live or through the production restore pipeline.
fn run_kiki_jiki_with_pause_carry(
    spec: KikiJikiBoard,
    carry: PauseCarry,
    mut answer: impl FnMut(&WaitingFor, &GameRunner, &[KikiJikiPrompt]) -> Option<GameAction>,
) -> Option<KikiJikiRun> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The target first, then Moonlit Meditation while the target is the only
    // artifact or creature P0 controls, so the Aura's own battlefield entry
    // attaches it there (CR 303.4f: P0 chooses a legal object to enchant, and
    // the target is the only one).
    let target = scenario.add_real_card(P0, spec.target, Zone::Battlefield, db);
    let moonlit = spec
        .moonlit
        .then(|| scenario.add_real_card(P0, MOONLIT_MEDITATION, Zone::Battlefield, db));
    let kiki = scenario.add_real_card(P0, KIKI_JIKI, Zone::Battlefield, db);
    for (player, name) in &spec.board {
        scenario.add_real_card(*player, name, Zone::Battlefield, db);
    }
    let alarm = spec
        .raise_the_alarm
        .then(|| scenario.add_real_card(P0, RAISE_THE_ALARM, Zone::Hand, db));
    if let Some(alarm) = alarm {
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::White, alarm, false, vec![]),
                ManaUnit::new(ManaType::Colorless, alarm, false, vec![]),
            ],
        );
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    if let Some(moonlit) = moonlit {
        assert_eq!(
            runner.state().objects[&moonlit].attached_to,
            Some(AttachTarget::Object(target)),
            "REACH-GUARD: Moonlit Meditation enchants the target"
        );
    }
    let pre: HashSet<ObjectId> = runner.state().battlefield.iter().copied().collect();
    if let Some(alarm) = alarm {
        runner
            .cast(alarm)
            .try_resolve()
            .expect("Raise the Alarm must be castable");
    }
    let soldiers: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| !pre.contains(id))
        .collect();
    if spec.raise_the_alarm {
        let halving = spec.board.contains(&(P1, HALVING_SEASON));
        assert_eq!(
            soldiers.len(),
            if halving { 1 } else { 2 },
            "REACH-GUARD: Raise the Alarm creates its Soldiers (halved by Halving Season)"
        );
        assert_eq!(
            runner.state().last_created_token_ids,
            soldiers,
            "REACH-GUARD: before the activation LastCreated names the Soldiers"
        );
    }
    let phase = runner.state().phase;
    let mut actions = vec![runner
        .activate(kiki, 0)
        .target_object(target)
        .try_resolve()
        .expect("Kiki-Jiki's ability must be activatable")
        .events()
        .to_vec()];
    let mut prompts: Vec<KikiJikiPrompt> = Vec::new();
    let mut restores = 0;
    let mut replacement_prompt_restores = 0;
    for _ in 0..MAX_PROMPTS {
        let waiting = runner.state().waiting_for.clone();
        if matches!(waiting, WaitingFor::Priority { .. })
            && runner.state().resolution_stack.is_empty()
        {
            break;
        }
        match carry {
            PauseCarry::Live => {}
            PauseCarry::SavedAndRestored => {
                assert!(
                    !matches!(waiting, WaitingFor::Priority { .. }),
                    "REACH-GUARD: restore only while paused at a prompt"
                );
                let frames = runner.state().resolution_stack.clone();
                let has_offered_batch = frames.iter().any(|frame| {
                    matches!(frame, ResolutionFrame::CopyToken(owner)
                        if owner.offered_batch == OfferedBatchCustody::AtReplacementPrompt)
                });
                runner = save_and_restore(&runner);
                restores += 1;
                if has_offered_batch {
                    replacement_prompt_restores += 1;
                }
                assert_eq!(runner.state().waiting_for, waiting);
                assert_eq!(runner.state().resolution_stack, frames);
            }
        }
        let candidates = match &waiting {
            WaitingFor::ReplacementChoice { candidates, .. } => candidates
                .iter()
                .map(|candidate| candidate.source_name.clone())
                .collect(),
            _ => Vec::new(),
        };
        let prompt = KikiJikiPrompt {
            kind: waiting.variant_name(),
            candidates,
            copy_token_frames: copy_token_frame_count(&runner),
        };
        let Some(action) = answer(&waiting, &runner, &prompts) else {
            break;
        };
        prompts.push(prompt);
        actions.push(runner.act(action).expect("answer must be accepted").events);
    }
    match carry {
        PauseCarry::Live => {}
        PauseCarry::SavedAndRestored => {
            assert_eq!(
                restores,
                prompts.len(),
                "every answered prompt was restored"
            );
            assert!(
                replacement_prompt_restores > 0,
                "REACH-GUARD: restored a CopyToken owner awaiting its substitute batch"
            );
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().resolution_stack.is_empty(),
        "CR 117.2e + CR 608.2: the activation finishes resolving in the answer to its last \
         prompt, waiting {:?}, frames {:?}, prompts {prompts:?}",
        runner.state().waiting_for,
        frame_kinds(&runner)
    );
    assert_eq!(
        runner.state().phase,
        phase,
        "CR 117.4 + CR 500.2: the phase cannot end while Kiki-Jiki's ability is resolving"
    );
    assert!(
        actions
            .last()
            .expect("at least the activation")
            .iter()
            .any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CopyTokenOf,
                    source_id,
                    ..
                } if *source_id == kiki
            )),
        "custody: Kiki-Jiki's copy owner completes in the action that settles the resolution, \
         prompts {prompts:?}"
    );
    let activation: Vec<GameEvent> = actions.iter().flatten().cloned().collect();
    assert!(
        activation.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::CopyTokenOf,
                source_id,
                ..
            } if *source_id == kiki
        )),
        "REACH-GUARD: the copy instruction resolved, prompts {prompts:?}, waiting {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| matches!(trigger.ability.effect, Effect::Sacrifice { .. })),
        "REACH-GUARD: \"Sacrifice it at the beginning of the next end step\" is installed"
    );
    let last_created_after_activation = runner.state().last_created_token_ids.clone();
    let token_names: Vec<String> = activation
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => {
                Some(runner.state().objects[object_id].name.clone())
            }
            _ => None,
        })
        .collect();
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    let mut end_step = Vec::new();
    for _ in 0..8 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        end_step.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority to resolve the end step's triggers")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "REACH-GUARD: the end step's triggers resolved, stack {:?}",
        runner.state().stack
    );
    Some(KikiJikiRun {
        runner,
        kiki,
        soldiers,
        activation,
        prompts,
        token_names,
        last_created_after_activation,
        end_step,
    })
}

fn sacrificed(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

/// R17: Halving Season makes Kiki-Jiki's one copy zero (⌊1 ÷ 2⌋), so no token
/// is created and "Sacrifice it at the beginning of the next end step" names
/// nothing (CR 609.3). The Soldier Raise the Alarm created earlier is not "it".
#[test]
fn r17_zero_token_copy_batch_republishes_nothing() {
    let Some(run) = run_kiki_jiki(true) else {
        return;
    };
    assert!(
        !run.activation
            .iter()
            .any(|event| matches!(event, GameEvent::TokenCreated { .. })),
        "REACH-GUARD: Halving Season makes the copy zero tokens"
    );
    assert_eq!(
        run.last_created_after_activation,
        Vec::<ObjectId>::new(),
        "a copy batch that created nothing publishes nothing to LastCreated"
    );
    let state = run.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the end step raises no prompt, got {:?}",
        state.waiting_for
    );
    let mut p0: Vec<String> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].controller == P0)
        .map(|id| state.objects[id].name.clone())
        .collect();
    p0.sort();
    let soldier = state.objects[&run.soldiers[0]].name.clone();
    let mut want = vec![BEARS.to_string(), KIKI_JIKI.to_string(), soldier];
    want.sort();
    assert_eq!(
        p0, want,
        "P0 keeps Kiki-Jiki, the Bears and the Soldier through the end step"
    );
    assert!(
        state.battlefield.contains(&run.soldiers[0]),
        "the Soldier is not \"it\""
    );
    assert_eq!(
        sacrificed(&run.end_step),
        Vec::<ObjectId>::new(),
        "nothing is sacrificed at the end step"
    );
}

/// R17 control: without Halving Season Kiki-Jiki creates its copy, LastCreated
/// names it, and the end step sacrifices it while the Soldiers survive.
#[test]
fn r17_control_copy_is_sacrificed_at_the_end_step() {
    let Some(run) = run_kiki_jiki(false) else {
        return;
    };
    let created: Vec<ObjectId> = run
        .activation
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(created.len(), 1, "REACH-GUARD: Kiki-Jiki creates one copy");
    assert_eq!(
        run.last_created_after_activation, created,
        "LastCreated names the copy"
    );
    let state = run.runner.state();
    assert_eq!(
        sacrificed(&run.end_step),
        created,
        "the end step sacrifices the copy"
    );
    assert!(!state.battlefield.contains(&created[0]), "the copy is gone");
    for soldier in &run.soldiers {
        assert!(
            state.battlefield.contains(soldier),
            "the Soldiers survive the end step"
        );
    }
}

/// R7k: Kiki-Jiki copies Metallic Mimic. The copy has Mimic's "As this
/// creature enters, choose a creature type" (CR 707.2), made before it enters
/// (CR 614.12a), so Kiki-Jiki's owner is parked at the choice. The choice's
/// answer enters the copy, and only then does the owner complete: "it" is the
/// Mimic copy, sacrificed at the next end step (CR 608.2c).
#[test]
fn r7k_as_enters_choice_on_kiki_jikis_copy_enters_before_the_owner_completes() {
    let run = run_kiki_jiki_with(
        KikiJikiBoard {
            target: METALLIC_MIMIC,
            board: vec![],
            moonlit: false,
            raise_the_alarm: false,
        },
        |waiting, _, _| match waiting {
            WaitingFor::NamedChoice { options, .. } => Some(GameAction::ChooseOption {
                choice: options[0].clone(),
            }),
            _ => None,
        },
    );
    let Some(run) = run else {
        return;
    };
    assert_eq!(
        run.prompts,
        vec![KikiJikiPrompt {
            kind: "NamedChoice",
            candidates: Vec::new(),
            copy_token_frames: 1,
        }],
        "REACH-GUARD: one prompt, the copy's as-enters creature-type choice, raised with \
         Kiki-Jiki's copy owner parked"
    );
    assert_eq!(
        run.token_names,
        vec![METALLIC_MIMIC.to_string()],
        "the one token is the Mimic copy"
    );
    assert_kiki_jiki_sacrifices_its_token(&run, METALLIC_MIMIC, &[KIKI_JIKI, METALLIC_MIMIC]);
}

/// The index of `source`'s accepting answer at an optional replacement prompt.
fn accept_index(candidates: &[ReplacementCandidateSummary], source: &str) -> usize {
    candidates
        .iter()
        .position(|candidate| candidate.source_name == source && candidate.description != "Decline")
        .unwrap_or_else(|| {
            panic!("REACH-GUARD: {source} offers an accepting answer, got {candidates:?}")
        })
}

/// The index of `source`'s declining answer at an optional replacement prompt.
fn decline_index(candidates: &[ReplacementCandidateSummary], source: &str) -> usize {
    candidates
        .iter()
        .position(|candidate| candidate.source_name == source && candidate.description == "Decline")
        .unwrap_or_else(|| {
            panic!("REACH-GUARD: {source} offers a Decline answer, got {candidates:?}")
        })
}

/// Whether every candidate of a prompt is `source`'s (its accept / decline pair).
fn only_from(candidates: &[ReplacementCandidateSummary], source: &str) -> bool {
    candidates
        .iter()
        .all(|candidate| candidate.source_name == source)
}

/// A Kiki-Jiki board whose target is enchanted by P0's Moonlit Meditation.
fn moonlit_kiki_board(target: &'static str, board: Vec<(PlayerId, &'static str)>) -> KikiJikiBoard {
    KikiJikiBoard {
        target,
        board,
        moonlit: true,
        raise_the_alarm: false,
    }
}

/// CR 614.6 + CR 608.2c: Kiki-Jiki's "it" is the one token its instruction
/// created, `want_name`: `LastCreated` names it once the activation resolves,
/// "Sacrifice it at the beginning of the next end step" sacrifices exactly it,
/// and P0 keeps exactly `survivors`. Kiki-Jiki's owner completes once, after
/// that token's `TokenCreated`.
fn assert_kiki_jiki_sacrifices_its_token(run: &KikiJikiRun, want_name: &str, survivors: &[&str]) {
    let created = run.created_tokens();
    assert_eq!(
        created.len(),
        1,
        "REACH-GUARD: one token is created (\"that many\" of Kiki-Jiki's one copy), got \
         {created:?}, prompts {:?}",
        run.prompts
    );
    let token = created[0];
    let token_created_at = run
        .activation
        .iter()
        .position(|event| matches!(event, GameEvent::TokenCreated { object_id, .. } if *object_id == token))
        .expect("REACH-GUARD: the token's TokenCreated");
    let kiki_terminals: Vec<usize> = run
        .activation
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CopyTokenOf,
                    source_id,
                    ..
                } if *source_id == run.kiki
            )
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        kiki_terminals.len(),
        1,
        "custody: Kiki-Jiki's copy owner completes once"
    );
    assert!(
        kiki_terminals[0] > token_created_at,
        "custody: Kiki-Jiki's owner completes after its token is created"
    );
    assert_eq!(
        run.last_created_after_activation, created,
        "LastCreated names the {want_name} token the instruction created"
    );
    assert_eq!(
        sacrificed(&run.end_step),
        created,
        "\"Sacrifice it\" sacrifices the {want_name} token at the next end step"
    );
    let state = run.runner.state();
    let mut p0: Vec<String> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].controller == P0)
        .map(|id| state.objects[id].name.clone())
        .collect();
    p0.sort();
    let mut want: Vec<String> = survivors.iter().map(|name| name.to_string()).collect();
    want.sort();
    assert_eq!(p0, want, "P0's battlefield after the end step");
}

/// R18: Kiki-Jiki copies the Grizzly Bears that P0's Moonlit Meditation
/// enchants, and P0 accepts Moonlit's "you may instead create that many tokens
/// that are copies of enchanted permanent". CR 614.6: Kiki-Jiki's creation
/// never happens and Moonlit's copy is created instead, so that copy is "it"
/// for "Sacrifice it at the beginning of the next end step".
#[test]
fn r18_moonlit_meditation_copy_is_kiki_jikis_it() {
    let run = run_kiki_jiki_with(
        moonlit_kiki_board(BEARS, Vec::new()),
        |waiting, _, prompts| {
            let WaitingFor::ReplacementChoice { candidates, .. } = waiting else {
                return None;
            };
            prompts.is_empty().then(|| GameAction::ChooseReplacement {
                index: accept_index(candidates, MOONLIT_MEDITATION),
            })
        },
    );
    let Some(run) = run else {
        return;
    };
    assert_eq!(
        run.prompts,
        vec![KikiJikiPrompt {
            kind: "ReplacementChoice",
            candidates: vec![
                MOONLIT_MEDITATION.to_string(),
                MOONLIT_MEDITATION.to_string()
            ],
            copy_token_frames: 1,
        }],
        "REACH-GUARD: one prompt, Moonlit Meditation's, raised with Kiki-Jiki's copy owner parked"
    );
    assert_eq!(
        run.token_names[0], BEARS,
        "REACH-GUARD: Moonlit's copy is a copy of the enchanted Bears"
    );
    assert_kiki_jiki_sacrifices_its_token(&run, "Moonlit", &[KIKI_JIKI, BEARS, MOONLIT_MEDITATION]);
}

/// R18 sibling: P0 declines Moonlit Meditation, so nothing is replaced and
/// Kiki-Jiki's own copy is created, named by "it" and sacrificed (CR 614.6).
#[test]
fn r18_sibling_declined_moonlit_meditation_keeps_kiki_jikis_copy() {
    let run = run_kiki_jiki_with(
        moonlit_kiki_board(BEARS, Vec::new()),
        |waiting, _, prompts| {
            let WaitingFor::ReplacementChoice { candidates, .. } = waiting else {
                return None;
            };
            prompts.is_empty().then(|| GameAction::ChooseReplacement {
                index: decline_index(candidates, MOONLIT_MEDITATION),
            })
        },
    );
    let Some(run) = run else {
        return;
    };
    assert_eq!(
        run.prompts.len(),
        1,
        "REACH-GUARD: Moonlit Meditation's prompt is the only one, saw {:?}",
        run.prompts
    );
    assert_kiki_jiki_sacrifices_its_token(
        &run,
        "Kiki-Jiki",
        &[KIKI_JIKI, BEARS, MOONLIT_MEDITATION],
    );
}

/// The answers on a Kiki-Jiki board with Moonlit Meditation and Jinnie Fay:
/// order Moonlit Meditation first, accept it, and decline Jinnie Fay on the
/// modified original event. On Moonlit's own copy event (raised with two
/// CopyToken owners parked) accept Jinnie Fay when `accept_jinnie`, taking the
/// Cat branch. `branch_frames` records the frames at each branch prompt.
fn moonlit_then_jinnie(
    accept_jinnie: bool,
    branch_frames: &mut Vec<Vec<FrameKind>>,
) -> impl FnMut(&WaitingFor, &GameRunner, &[KikiJikiPrompt]) -> Option<GameAction> + '_ {
    move |waiting, runner, _| match waiting {
        WaitingFor::ReplacementChoice {
            candidates, kind, ..
        } => {
            let index = match kind {
                ReplacementChoiceKind::Order => candidates
                    .iter()
                    .position(|candidate| candidate.source_name == MOONLIT_MEDITATION)
                    .expect("REACH-GUARD: the ordering offers Moonlit Meditation"),
                _ if only_from(candidates, MOONLIT_MEDITATION) => {
                    accept_index(candidates, MOONLIT_MEDITATION)
                }
                _ if accept_jinnie && copy_token_frame_count(runner) == 2 => {
                    accept_index(candidates, JINNIE)
                }
                _ => decline_index(candidates, JINNIE),
            };
            Some(GameAction::ChooseReplacement { index })
        }
        WaitingFor::ChooseOneOfBranch { .. } => {
            branch_frames.push(frame_kinds(runner));
            Some(GameAction::ChooseBranch { index: 0 })
        }
        _ => None,
    }
}

/// The Jinnie Fay prompt raised on Moonlit Meditation's own copy event, while
/// Moonlit's copy owner is parked above Kiki-Jiki's.
fn assert_jinnie_prompted_on_moonlits_copy_event(run: &KikiJikiRun) {
    assert!(
        run.prompts
            .iter()
            .any(|prompt| prompt.kind == "ReplacementChoice"
                && prompt.copy_token_frames == 2
                && prompt.candidates.iter().all(|name| name == JINNIE)),
        "REACH-GUARD: Jinnie Fay prompts on Moonlit's copy event with two CopyToken owners \
         parked, saw {:?}",
        run.prompts
    );
}

/// R19: R18's board plus P0's Jinnie Fay. Moonlit Meditation is ordered first
/// and accepted; Jinnie Fay is declined on the modified event and again on
/// Moonlit's copy event, which pauses with Moonlit's own copy owner above
/// Kiki-Jiki's. Moonlit's copy is still Kiki-Jiki's "it" (CR 614.6).
#[test]
fn r19_moonlit_copy_paused_inside_a_declined_jinnie_prompt_is_kiki_jikis_it() {
    let mut branch_frames = Vec::new();
    let run = run_kiki_jiki_with(
        moonlit_kiki_board(BEARS, vec![(P0, JINNIE)]),
        moonlit_then_jinnie(false, &mut branch_frames),
    );
    let Some(run) = run else {
        return;
    };
    assert_jinnie_prompted_on_moonlits_copy_event(&run);
    assert!(
        branch_frames.is_empty(),
        "REACH-GUARD: no Jinnie Fay substitution was accepted"
    );
    assert_eq!(
        run.token_names[0], BEARS,
        "REACH-GUARD: the token is Moonlit's copy of the Bears"
    );
    assert_kiki_jiki_sacrifices_its_token(
        &run,
        "Moonlit",
        &[KIKI_JIKI, BEARS, MOONLIT_MEDITATION, JINNIE],
    );
}

/// R19b: Kiki-Jiki copies a Triskelion that Moonlit Meditation enchants, under
/// Hardened Scales and Branching Evolution. Moonlit is accepted; its copy's
/// "enters with three +1/+1 counters" pauses on the counter ordering with
/// Moonlit's copy owner above Kiki-Jiki's. The copy is Kiki-Jiki's "it"
/// (CR 614.6).
#[test]
fn r19b_moonlit_copy_paused_on_a_counter_prompt_is_kiki_jikis_it() {
    let run = run_kiki_jiki_with(
        moonlit_kiki_board(TRISKELION, counter_board()),
        |waiting, _, _| match waiting {
            WaitingFor::ReplacementChoice { candidates, .. }
                if only_from(candidates, MOONLIT_MEDITATION) =>
            {
                Some(GameAction::ChooseReplacement {
                    index: accept_index(candidates, MOONLIT_MEDITATION),
                })
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let index = PREFERENCE
                    .iter()
                    .find_map(|preferred| {
                        candidates
                            .iter()
                            .position(|candidate| candidate.source_name == *preferred)
                    })
                    .expect("REACH-GUARD: the counter ordering offers Hardened Scales");
                Some(GameAction::ChooseReplacement { index })
            }
            _ => None,
        },
    );
    let Some(run) = run else {
        return;
    };
    assert!(
        run.prompts
            .iter()
            .any(|prompt| prompt.kind == "ReplacementChoice"
                && prompt.copy_token_frames == 2
                && prompt.candidates.iter().any(|name| name == HARDENED_SCALES)),
        "REACH-GUARD: the copy's counter ordering is raised with two CopyToken owners parked, \
         saw {:?}",
        run.prompts
    );
    assert_eq!(
        run.token_names[0], TRISKELION,
        "REACH-GUARD: the token is Moonlit's copy of the Triskelion"
    );
    assert_kiki_jiki_sacrifices_its_token(
        &run,
        "Moonlit",
        &[
            KIKI_JIKI,
            TRISKELION,
            MOONLIT_MEDITATION,
            HARDENED_SCALES,
            BRANCHING_EVOLUTION,
        ],
    );
}

/// R22: R19's board with Jinnie Fay accepted on Moonlit's copy event (the Cat
/// branch). Two substitutions: the Cat replaced Moonlit's copy, which replaced
/// Kiki-Jiki's, so the Cat is "it" (CR 614.6 twice).
#[test]
fn r22_jinnie_cat_replacing_moonlits_copy_is_kiki_jikis_it() {
    let mut branch_frames = Vec::new();
    let run = run_kiki_jiki_with(
        moonlit_kiki_board(BEARS, vec![(P0, JINNIE)]),
        moonlit_then_jinnie(true, &mut branch_frames),
    );
    let Some(run) = run else {
        return;
    };
    assert_jinnie_prompted_on_moonlits_copy_event(&run);
    // The substitute's drain went into the frame beneath Moonlit's copy owner,
    // which the zero-token delivery of Moonlit's replaced event re-parked on
    // top: that delivery completed having created nothing.
    assert_eq!(
        branch_frames,
        vec![vec![
            FrameKind::AbilityContinuation,
            FrameKind::CopyToken,
            FrameKind::PostReplacement,
            FrameKind::CopyToken,
        ]],
        "REACH-GUARD: one branch prompt, raised with Moonlit's copy owner on top"
    );
    assert_eq!(
        run.token_names[0], "Cat",
        "REACH-GUARD: the token is Jinnie Fay's Cat"
    );
    assert_kiki_jiki_sacrifices_its_token(
        &run,
        "Cat",
        &[KIKI_JIKI, BEARS, MOONLIT_MEDITATION, JINNIE],
    );
}

/// R22s: R22's nonempty Cat substitute crosses save/restore at every prompt.
/// The restored owners adopt the twice-replaced creation's Cat (CR 614.6),
/// completing resolution without a priority window (CR 117.2e).
#[test]
fn r22s_jinnie_cat_replacing_moonlits_copy_is_kiki_jikis_it_across_save_and_restore() {
    let mut branch_frames = Vec::new();
    let run = run_kiki_jiki_with_pause_carry(
        moonlit_kiki_board(BEARS, vec![(P0, JINNIE)]),
        PauseCarry::SavedAndRestored,
        moonlit_then_jinnie(true, &mut branch_frames),
    );
    let Some(run) = run else {
        return;
    };
    assert_jinnie_prompted_on_moonlits_copy_event(&run);
    assert_eq!(
        branch_frames,
        vec![vec![
            FrameKind::AbilityContinuation,
            FrameKind::CopyToken,
            FrameKind::PostReplacement,
            FrameKind::CopyToken,
        ]],
        "REACH-GUARD: one branch prompt, raised with Moonlit's copy owner on top"
    );
    assert_eq!(
        run.token_names[0], "Cat",
        "REACH-GUARD: the token is Jinnie Fay's Cat"
    );
    assert_kiki_jiki_sacrifices_its_token(
        &run,
        "Cat",
        &[KIKI_JIKI, BEARS, MOONLIT_MEDITATION, JINNIE],
    );
}

/// R22p: R22's board and answers, with passing priority allowed whenever a
/// priority window opens while the resolution is still under way. No player
/// receives priority while Kiki-Jiki's ability resolves (CR 117.2e), so no
/// such window opens, and the main phase cannot end mid-resolution
/// (CR 117.4 + CR 500.2).
#[test]
fn r22p_no_priority_window_while_kiki_jikis_ability_resolves() {
    let mut branch_frames = Vec::new();
    let mut stranded: Vec<(Phase, Vec<FrameKind>)> = Vec::new();
    let mut answer = moonlit_then_jinnie(true, &mut branch_frames);
    let run = run_kiki_jiki_with(
        moonlit_kiki_board(BEARS, vec![(P0, JINNIE)]),
        |waiting, runner, prompts| {
            if matches!(waiting, WaitingFor::Priority { .. })
                && !runner.state().resolution_stack.is_empty()
            {
                stranded.push((runner.state().phase, frame_kinds(runner)));
                return Some(GameAction::PassPriority);
            }
            answer(waiting, runner, prompts)
        },
    );
    let Some(run) = run else {
        return;
    };
    assert!(
        stranded.is_empty(),
        "no player receives priority while Kiki-Jiki's ability resolves, passes taken at \
         {stranded:?}"
    );
    assert_eq!(
        run.token_names,
        vec!["Cat".to_string()],
        "REACH-GUARD: the token is Jinnie Fay's Cat"
    );
    assert_kiki_jiki_sacrifices_its_token(
        &run,
        "Cat",
        &[KIKI_JIKI, BEARS, MOONLIT_MEDITATION, JINNIE],
    );
}

/// R20: Twinflame on two Grizzly Bears with P0's Esix, Fractal Bloom. Esix is
/// accepted on the first copy's event and the second Bears is chosen, so the
/// first batch's token is Esix's copy (CR 614.6), and it is one of "those
/// tokens" (CR 603.7c). The second batch is created unreplaced: Esix applies
/// the first time each turn.
#[test]
fn r20_esix_copy_joins_those_tokens() {
    // Each prompt answered: its kind, and the owner's offered batch then.
    let mut prompts: Vec<(&'static str, Option<OfferedBatchCustody>)> = Vec::new();
    let driven = cast_twinflame_with(&[BEARS, BEARS], &[(P0, ESIX)], |waiting, driven| {
        let action = match waiting {
            WaitingFor::ReplacementChoice { candidates, .. } => GameAction::ChooseReplacement {
                index: accept_index(candidates, ESIX),
            },
            WaitingFor::CopyTargetChoice { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(driven.sources[1])),
            },
            _ => return None,
        };
        prompts.push((
            waiting.variant_name(),
            copy_token_owner(&driven.runner).map(|owner| owner.offered_batch),
        ));
        Some(action)
    });
    let Some(driven) = driven else {
        return;
    };
    let marked = Some(OfferedBatchCustody::AtReplacementPrompt);
    assert_eq!(
        prompts,
        vec![("ReplacementChoice", marked), ("CopyTargetChoice", marked)],
        "REACH-GUARD: one Esix prompt and one copy-source choice, each with the owner parked \
         and its first batch marked"
    );
    let tokens = driven.created_tokens();
    assert_eq!(
        tokens.iter().collect::<HashSet<_>>().len(),
        2,
        "REACH-GUARD: Esix's copy and the second batch's copy, two distinct tokens, got {tokens:?}"
    );
    assert_eq!(
        named(&driven, BEARS),
        tokens,
        "REACH-GUARD: both tokens are Grizzly Bears copies"
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated lists Esix's copy and the second copy, in TokenCreated order"
    );
    let members = assert_referent_is_every_token(&driven);
    assert_eq!(
        members, tokens,
        "the referent lists both copies in TokenCreated order"
    );
    assert_end_step_exiles_every_token(driven);
}

/// R21: Twinflame on two Grizzly Bears with P0's Jinnie Fay, accepted at the
/// first copy's event with the Cat branch. Jinnie Fay's ruling: "Anything else
/// specified in the effect creating the tokens ... still applies", so the
/// Cats are among "those tokens" (CR 614.6). The token-choice seed keeps Jinnie
/// Fay off the second copy's event, which creates a Bears copy.
#[test]
fn r21_jinnie_cat_is_one_of_those_tokens() {
    r21_jinnie_cat(PauseCarry::Live);
}

/// R21 through the engine's save/restore pipeline at the Jinnie Fay prompt and
/// at the branch prompt, while the owner's first batch is marked.
#[test]
fn r21_jinnie_cat_is_one_of_those_tokens_across_save_and_restore() {
    r21_jinnie_cat(PauseCarry::SavedAndRestored);
}

fn r21_jinnie_cat(carry: PauseCarry) {
    let mut prompts: Vec<&'static str> = Vec::new();
    let mut restores = 0;
    let driven = cast_twinflame_with(&[BEARS, BEARS], &[(P0, JINNIE)], |waiting, driven| {
        let action = match waiting {
            WaitingFor::ReplacementChoice { candidates, .. } => GameAction::ChooseReplacement {
                index: accept_index(candidates, JINNIE),
            },
            WaitingFor::ChooseOneOfBranch { .. } => GameAction::ChooseBranch { index: 0 },
            _ => return None,
        };
        prompts.push(waiting.variant_name());
        let owner = copy_token_owner(&driven.runner)
            .cloned()
            .expect("REACH-GUARD: the CopyToken owner is parked at every Jinnie Fay prompt");
        assert_eq!(
            owner.offered_batch,
            OfferedBatchCustody::AtReplacementPrompt,
            "the owner's first batch is owed the tokens of whatever replaces its creation"
        );
        if let PauseCarry::SavedAndRestored = carry {
            driven.runner = save_and_restore(&driven.runner);
            restores += 1;
            assert_eq!(
                copy_token_owner(&driven.runner).cloned(),
                Some(owner),
                "the owner, its marked batch included, crosses the wire unchanged"
            );
            assert_eq!(
                driven.runner.state().waiting_for,
                *waiting,
                "the restored game is at the same prompt"
            );
        }
        Some(action)
    });
    let Some(driven) = driven else {
        return;
    };
    assert_eq!(
        prompts,
        vec!["ReplacementChoice", "ChooseOneOfBranch"],
        "REACH-GUARD: one Jinnie Fay prompt and one branch prompt"
    );
    if let PauseCarry::SavedAndRestored = carry {
        assert_eq!(
            restores, 2,
            "REACH-GUARD: saved and restored at both prompts"
        );
    }
    let tokens = driven.created_tokens();
    let bears = named(&driven, BEARS);
    assert_eq!(
        bears.len(),
        1,
        "REACH-GUARD: the second batch is one Bears copy, got {tokens:?}"
    );
    // Measured: the Cat branch creates no Cat ("that many" reads zero), a
    // count defect outside this row. The row asserts only that whatever the
    // substitute created is a Cat and is among "those tokens".
    assert!(
        tokens
            .iter()
            .filter(|id| !bears.contains(id))
            .all(|id| driven.runner.state().objects[id].name == "Cat"),
        "every substitute token is Jinnie Fay's Cat, got {tokens:?}"
    );
    assert_eq!(
        driven.runner.state().last_created_token_ids,
        tokens,
        "LastCreated lists every created token in TokenCreated order"
    );
    assert_referent_is_every_token(&driven);
    assert_end_step_exiles_every_token(driven);
}
