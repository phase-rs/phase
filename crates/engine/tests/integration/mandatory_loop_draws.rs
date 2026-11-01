//! CR 104.4b + CR 732.4: a loop of mandatory actions is a draw, whether the players pass
//! through it or take the auto-pass shortcut over it (CR 732.1), and even when each cycle
//! only re-mints identities, discloses a card, or asks a choice with one legal option. A
//! loop holding an optional action — a "may" answered or remembered, a remembered optional
//! replacement, or a choice one of whose options ends it — is not; one holding an out or a
//! choice every option of which continues it is offered.

use std::collections::BTreeMap;

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{CountedTake, ShortcutResponse, UnperformableCause};
use engine::game::engine::{apply, EngineError};
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::perf_counters::play_trace_counters;
use engine::game::period_confirm::mandatory_period_holds_for_tests;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{ChosenAttribute, TargetRef};
use engine::types::actions::{GameAction, PriorityYieldOp, ReplacementAutoChoice};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoMayChoice, AutoPassRequest, CastPaymentMode, GameEnd, GameState, LoopDetectionMode,
    MayTriggerAutoChoiceScope, PersistedGameState, StackEntryKind, WaitingFor, YieldScope,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionAvailability, InteractionOpportunityResponse, InteractionResponse,
    InteractionResponseSpec, InteractionSessionId, InteractionShortcutCountSpec,
    InteractionShortcutDecision, InteractionSubmission,
};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drive {
    Pass,
    UntilTurnBoundary,
    UntilStackEmpty,
    /// P0 stands on UntilStackEmpty and P1 passes.
    P0UntilStackEmpty,
    /// Both players pass until the turn boundary, and P1 yields to each of P0's triggers as
    /// it reaches the top of the stack, so one action drives many cycles.
    FullYields,
}

/// How P0 answers Leonin Snarecaster's "may": at every prompt, or once, remembered.
#[derive(Clone, Copy, Debug)]
enum May {
    Prompted { accept: bool },
    Remembered { accept: bool },
}

/// Which option P0 takes at a target or card choice among several.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Pick {
    #[default]
    First,
    /// The option after the one the previous choice took, by position.
    Alternate,
    /// The option with this name.
    Named(&'static str),
}

impl Pick {
    /// The position of the option this pick takes among `names`, at the run's `made`th choice.
    fn index(self, names: &[String], made: usize) -> usize {
        match self {
            Pick::First => 0,
            Pick::Alternate => made % names.len(),
            Pick::Named(name) => names
                .iter()
                .position(|option| option == name)
                .expect("the named option is offered"),
        }
    }
}

const EVERY_DRIVE: [Drive; 3] = [
    Drive::Pass,
    Drive::UntilTurnBoundary,
    Drive::UntilStackEmpty,
];

#[derive(Clone, Copy, Debug, Default)]
struct Board {
    /// This many Brain Maggots on P0's battlefield, and a Lightning Bolt in P1's hand.
    maggots: usize,
    /// A Murder in P1's hand.
    murder: bool,
    /// Oblivion Ring on P0's battlefield.
    oring: bool,
    /// How P0 answers a target or card choice among several.
    pick: Pick,
    /// Animate Dead targets Grizzly Bears instead of Worldgorger Dragon.
    bears: bool,
    /// P1 has a Mountain on the battlefield and a Lightning Bolt in hand.
    optional: bool,
    /// Leonin Snarecaster on P0's battlefield.
    snarecaster: Option<May>,
    /// Gibbering Fiend on P0's battlefield and Battletide Alchemist on P1's, whose
    /// prevention P1 remembers at its first prompt.
    fiend: bool,
    /// Banisher Priest on P0's battlefield and this many Grizzly Bears on P1's.
    banisher: Option<usize>,
    /// P0 has a Mountain on the battlefield and a Lightning Bolt in hand.
    p0_out: bool,
    /// Marauding Raptor on P0's battlefield and Polyraptor cast from P0's hand, in place of
    /// Worldgorger Dragon and Animate Dead.
    raptor: bool,
    /// Soul Warden on this player's battlefield.
    soul_warden: Option<PlayerId>,
    /// Animate Dead returns Grizzly Bears, P0 casts Worldgorger Dragon, which exiles both, and on
    /// P1's turn P1 casts Murder on the Dragon, so the loop runs on P1's turn.
    opponents_turn: bool,
    /// With `opponents_turn`, the Lightning Bolts P1 casts at P0, holding priority, before Murder.
    opponents_bolts: usize,
    /// Prodigal Sorcerer on P1's battlefield.
    p1_sorcerer: bool,
    /// Lumberknot on P1's battlefield.
    lumberknot: bool,
    /// A 50/50 on P1's battlefield with this Oracle text, holding this many -1/-1 counters.
    sink: Option<(&'static str, u32)>,
    /// Grizzly Bears on P1's battlefield.
    p1_bears: bool,
    /// Poison-Tip Archer on P1's battlefield.
    archer: bool,
    /// Rest in Peace on P0's battlefield.
    rest_in_peace: bool,
    /// This many Grizzly Bears on P0's battlefield.
    p0_bears: usize,
    /// Ichor Rats on P0's battlefield.
    ichor_rats: bool,
    /// The card whose trigger P0 orders to resolve first; otherwise Animate Dead's resolves last.
    first: Option<&'static str>,
    /// Wood Elves on P0's battlefield, and this many Swamps, no Forest, in P0's library.
    wood_elves: Option<usize>,
    /// Impact Tremors on P0's battlefield.
    tremors: bool,
}

#[derive(Debug, PartialEq)]
enum Terminal {
    GameOver(Option<PlayerId>),
    SettledOnEmptyStack,
    BeatsExhausted,
    Offer {
        predicted_winner: Option<PlayerId>,
        mandatory: bool,
    },
    OtherPrompt(String),
}

struct Run {
    terminal: Terminal,
    runner: GameRunner,
    target: ObjectId,
    /// Times the watched card entered the battlefield, at the end and at half time.
    entries: usize,
    entries_at_half: usize,
    /// The most entries one action produced.
    max_entries_per_action: usize,
    reveals: usize,
    reveals_at_half: usize,
    max_ring: usize,
    /// Times each prompt other than priority was answered.
    prompts: BTreeMap<&'static str, usize>,
    /// Priority beats at which P0's Mountain stood untapped on the battlefield.
    out_beats: usize,
    /// Each action's acting player, whether it ordered triggers, and the confirm replays it ran.
    replays: Vec<(Option<PlayerId>, bool, u64)>,
}

impl Run {
    fn prompted(&self, prompt: &str) -> usize {
        self.prompts.get(prompt).copied().unwrap_or(0)
    }

    fn state(&self) -> &GameState {
        self.runner.state()
    }

    /// The permanents named `name` on the battlefield.
    fn on_battlefield(&self, name: &str) -> Vec<&GameObject> {
        let state = self.state();
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.name == name)
            .collect()
    }

    /// The `counter`s on the permanent named `name`.
    fn counters_on(&self, name: &str, counter: CounterType) -> u32 {
        self.on_battlefield(name)
            .first()
            .and_then(|object| object.counters.get(&counter).copied())
            .unwrap_or(0)
    }

    fn poison(&self, player: PlayerId) -> u32 {
        self.state().players[player.0 as usize].poison_counters
    }

    fn life(&self, player: PlayerId) -> i32 {
        self.state().players[player.0 as usize].life
    }
}

/// The objects a board's drive answers prompts about.
struct Built {
    runner: GameRunner,
    /// The card whose entries are counted: the reanimation target, or Polyraptor.
    watched: ObjectId,
    animate: Option<ObjectId>,
    p0_mountain: Option<ObjectId>,
}

/// Builds `board` and casts its kickoff spell: Animate Dead on Worldgorger Dragon (or Grizzly
/// Bears), or Polyraptor.
fn build(board: Board, mode: LoopDetectionMode) -> Option<Built> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kickoff = if board.raptor {
        scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
        let polyraptor = scenario.add_real_card(P0, "Polyraptor", Zone::Hand, db);
        let mut pool = vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ];
        pool.extend((0..6).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])));
        scenario.with_mana_pool(P0, pool);
        (polyraptor, None)
    } else {
        let target_name = if board.bears || board.opponents_turn {
            "Grizzly Bears"
        } else {
            "Worldgorger Dragon"
        };
        let target = scenario.add_real_card(P0, target_name, Zone::Graveyard, db);
        let animate = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
        scenario.with_mana_pool(
            P0,
            (0..2)
                .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
                .collect(),
        );
        (target, Some(animate))
    };
    if board.maggots > 0 {
        for _ in 0..board.maggots {
            scenario.add_real_card(P0, "Brain Maggot", Zone::Battlefield, db);
        }
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    }
    if board.murder {
        scenario.add_real_card(P1, "Murder", Zone::Hand, db);
    }
    if board.oring {
        scenario.add_real_card(P0, "Oblivion Ring", Zone::Battlefield, db);
    }
    if board.optional {
        scenario.add_real_card(P1, "Mountain", Zone::Battlefield, db);
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    }
    if board.snarecaster.is_some() {
        scenario.add_real_card(P0, "Leonin Snarecaster", Zone::Battlefield, db);
    }
    if board.fiend {
        scenario.add_real_card(P0, "Gibbering Fiend", Zone::Battlefield, db);
        scenario.add_real_card(P1, "Battletide Alchemist", Zone::Battlefield, db);
    }
    if board.tremors {
        scenario.add_real_card(P0, "Impact Tremors", Zone::Battlefield, db);
    }
    if let Some(bears) = board.banisher {
        scenario.add_real_card(P0, "Banisher Priest", Zone::Battlefield, db);
        for _ in 0..bears {
            scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        }
    }
    let p0_mountain = board.p0_out.then(|| {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db)
    });
    if let Some(seat) = board.soul_warden {
        scenario.add_real_card(seat, "Soul Warden", Zone::Battlefield, db);
    }
    let opponents_turn = board.opponents_turn.then(|| {
        let dragon = scenario.add_real_card(P0, "Worldgorger Dragon", Zone::Hand, db);
        let murder = scenario.add_real_card(P1, "Murder", Zone::Hand, db);
        let bolts: Vec<ObjectId> = (0..board.opponents_bolts)
            .map(|_| scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db))
            .collect();
        (dragon, murder, bolts)
    });
    if board.p1_sorcerer {
        scenario.add_real_card(P1, "Prodigal Sorcerer", Zone::Battlefield, db);
    }
    if board.lumberknot {
        scenario.add_real_card(P1, "Lumberknot", Zone::Battlefield, db);
    }
    let sink = board.sink.map(|(text, counters)| {
        let sink = scenario
            .add_creature_from_oracle(P1, "Test Counter Sink", 50, 50, text)
            .id();
        (sink, counters)
    });
    if board.p1_bears {
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    }
    if board.archer {
        scenario.add_real_card(P1, "Poison-Tip Archer", Zone::Battlefield, db);
    }
    if board.ichor_rats {
        scenario.add_real_card(P0, "Ichor Rats", Zone::Battlefield, db);
    }
    if board.rest_in_peace {
        scenario.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    }
    for _ in 0..board.p0_bears {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    }
    if board.wood_elves.is_some() {
        scenario.add_real_card(P0, "Wood Elves", Zone::Battlefield, db);
    }
    for (seat, swamps) in [(P0, board.wood_elves.unwrap_or(10)), (P1, 10)] {
        for _ in 0..swamps {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    if let Some((sink, counters)) = sink.filter(|&(_, counters)| counters > 0) {
        runner
            .state_mut()
            .objects
            .get_mut(&sink)
            .expect("the sink is on the battlefield")
            .counters
            .insert(CounterType::Minus1Minus1, counters);
    }
    let (mut watched, animate) = kickoff;
    match animate {
        Some(animate) => drop(runner.cast(animate).target_object(watched).commit()),
        None => drop(runner.cast(watched).commit()),
    }
    if let Some((dragon, murder, bolts)) = opponents_turn {
        settle(&mut runner, |state| holds_priority(state, P0));
        add_mana(&mut runner, P0, &[ManaType::Red; 3]);
        add_mana(&mut runner, P0, &[ManaType::Colorless; 3]);
        drop(runner.cast(dragon).commit());
        settle(&mut runner, |state| holds_priority(state, P0));
        settle(&mut runner, |state| {
            state.active_player == P1
                && state.phase == Phase::PreCombatMain
                && holds_priority(state, P1)
        });
        add_mana(&mut runner, P1, &[ManaType::Black; 2]);
        add_mana(&mut runner, P1, &[ManaType::Colorless]);
        for bolt in bolts {
            add_mana(&mut runner, P1, &[ManaType::Red]);
            drop(runner.cast(bolt).target_player(P0).commit());
        }
        drop(runner.cast(murder).target_object(dragon).commit());
        watched = dragon;
    }
    Some(Built {
        runner,
        watched,
        animate,
        p0_mountain,
    })
}

/// Whether `player` holds priority on an empty stack.
fn holds_priority(state: &GameState, player: PlayerId) -> bool {
    state.stack.is_empty()
        && matches!(state.waiting_for, WaitingFor::Priority { player: holder } if holder == player)
}

/// Passes priority until `done` holds.
fn settle(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool) {
    for _ in 0..200 {
        if done(runner.state()) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("each player may pass priority");
    }
    panic!("the setup did not reach its frame");
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, kinds: &[ManaType]) {
    for &kind in kinds {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(kind, ObjectId(0), false, vec![]));
    }
}

fn run(board: Board, mode: LoopDetectionMode, drive: Drive, beats: usize) -> Option<Run> {
    run_observed(board, mode, drive, beats, |_| {})
}

/// [`run`], showing `observe` every frame before it is answered.
fn run_observed(
    board: Board,
    mode: LoopDetectionMode,
    drive: Drive,
    beats: usize,
    mut observe: impl FnMut(&GameState),
) -> Option<Run> {
    let Built {
        mut runner,
        watched: target,
        animate,
        p0_mountain,
    } = build(board, mode)?;

    let (mut entries, mut entries_at_half, mut reveals, mut reveals_at_half, mut max_ring) =
        (0, 0, 0, 0, 0);
    let (mut max_entries_per_action, mut out_beats, mut remembered) = (0, 0, false);
    let mut picks = 0;
    let mut prompts = BTreeMap::new();
    let mut replays = Vec::new();
    let mut terminal = Terminal::BeatsExhausted;
    for beat in 0..beats {
        if beat == beats / 2 {
            (entries_at_half, reveals_at_half) = (entries, reveals);
        }
        let state = runner.state();
        observe(state);
        let mut yield_first = None;
        max_ring = max_ring.max(state.loop_detect_ring.len());
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            *prompts.entry(state.waiting_for.variant_name()).or_insert(0) += 1;
        }
        let action = match &state.waiting_for {
            WaitingFor::GameOver { winner } => {
                terminal = Terminal::GameOver(*winner);
                break;
            }
            WaitingFor::LoopShortcut {
                predicted_winner,
                certificate,
                ..
            } => {
                terminal = Terminal::Offer {
                    predicted_winner: *predicted_winner,
                    mandatory: certificate.mandatory,
                };
                break;
            }
            WaitingFor::Priority { .. } if state.stack.is_empty() => {
                terminal = Terminal::SettledOnEmptyStack;
                break;
            }
            WaitingFor::Priority { player } => {
                if p0_mountain.is_some_and(|mountain| {
                    let mountain = &state.objects[&mountain];
                    mountain.zone == Zone::Battlefield && !mountain.tapped
                }) {
                    out_beats += 1;
                }
                match drive {
                    Drive::Pass => GameAction::PassPriority,
                    Drive::UntilTurnBoundary => until_turn_boundary(),
                    Drive::UntilStackEmpty => until_stack_empty(),
                    Drive::P0UntilStackEmpty if *player == P0 => until_stack_empty(),
                    Drive::P0UntilStackEmpty => GameAction::PassPriority,
                    Drive::FullYields => {
                        yield_first = state.stack.back().and_then(|top| match &top.kind {
                            StackEntryKind::TriggeredAbility { source_id, .. }
                                if *player == P1
                                    && top.controller == P0
                                    && !state.is_priority_yielded(P1, top) =>
                            {
                                Some(*source_id)
                            }
                            _ => None,
                        });
                        until_turn_boundary()
                    }
                }
            }
            // The trigger listed last goes on the stack last and resolves first.
            WaitingFor::OrderTriggers { triggers, .. } => {
                let mut order: Vec<usize> = (0..triggers.len()).collect();
                order.sort_by_key(|&index| {
                    let source = triggers[index].source_id;
                    match board.first {
                        Some(name) => state.objects.get(&source).is_some_and(|o| o.name == name),
                        None => Some(source) != animate,
                    }
                });
                GameAction::OrderTriggers { order }
            }
            WaitingFor::RevealChoice { cards, .. } => {
                reveals += 1;
                let names: Vec<String> = cards
                    .iter()
                    .map(|card| state.objects[card].name.clone())
                    .collect();
                let card = cards[board.pick.index(&names, picks)];
                picks += 1;
                GameAction::SelectCards { cards: vec![card] }
            }
            WaitingFor::OptionalEffectChoice {
                may_trigger_key,
                same_card_may_trigger_choice_available,
                ..
            } => match board.snarecaster {
                Some(May::Remembered { accept }) if !remembered && may_trigger_key.is_some() => {
                    remembered = true;
                    GameAction::DecideOptionalEffectAndRemember {
                        choice: if accept {
                            AutoMayChoice::Accept
                        } else {
                            AutoMayChoice::Decline
                        },
                        scope: if *same_card_may_trigger_choice_available {
                            MayTriggerAutoChoiceScope::SameCard
                        } else {
                            MayTriggerAutoChoiceScope::ExactInstance
                        },
                    }
                }
                Some(May::Prompted { accept } | May::Remembered { accept }) => {
                    GameAction::DecideOptionalEffect { accept }
                }
                None => GameAction::DecideOptionalEffect { accept: false },
            },
            WaitingFor::ReplacementChoice { .. } if board.fiend && !remembered => {
                remembered = true;
                GameAction::ChooseReplacementAndRemember {
                    choice: ReplacementAutoChoice::Optional { index: 0 },
                }
            }
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::TriggerTargetSelection { selection, .. } if board.pick != Pick::First => {
                let targets = &selection.current_legal_targets;
                let names: Vec<String> = targets
                    .iter()
                    .map(|target| match target {
                        TargetRef::Object(id) => state.objects[id].name.clone(),
                        TargetRef::Player(player) => format!("{player:?}"),
                    })
                    .collect();
                let target = targets[board.pick.index(&names, picks)].clone();
                picks += 1;
                GameAction::ChooseTarget {
                    target: Some(target),
                }
            }
            WaitingFor::TriggerTargetSelection { .. } => engine::ai_support::legal_actions(state)
                .into_iter()
                .next()
                .expect("the trigger has a target"),
            other => {
                terminal = Terminal::OtherPrompt(format!("{other:?}"));
                break;
            }
        };
        if let Some(source_id) = yield_first {
            runner
                .act(GameAction::SetPriorityYield {
                    op: PriorityYieldOp::Add {
                        source_id,
                        scope: YieldScope::AllCopies,
                    },
                })
                .expect("P1 may yield to a trigger on the stack");
        }
        let acting = runner.state().waiting_for.acting_player();
        let ordered = matches!(action, GameAction::OrderTriggers { .. });
        let drives_before = play_trace_counters().confirm_drives;
        let result = runner.act(action).expect("every driven action is legal");
        replays.push((
            acting,
            ordered,
            play_trace_counters().confirm_drives - drives_before,
        ));
        let entered = result
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
                        if *object_id == target
                )
            })
            .count();
        entries += entered;
        max_entries_per_action = max_entries_per_action.max(entered);
    }
    Some(Run {
        terminal,
        runner,
        target,
        entries,
        entries_at_half,
        max_entries_per_action,
        reveals,
        reveals_at_half,
        max_ring,
        prompts,
        out_beats,
        replays,
    })
}

fn until_turn_boundary() -> GameAction {
    GameAction::SetAutoPass {
        mode: AutoPassRequest::UntilTurnBoundary {
            until: Default::default(),
        },
    }
}

fn until_stack_empty() -> GameAction {
    GameAction::SetAutoPass {
        mode: AutoPassRequest::UntilStackEmpty,
    }
}

const PLAIN: Board = Board {
    maggots: 0,
    murder: false,
    oring: false,
    pick: Pick::First,
    bears: false,
    optional: false,
    snarecaster: None,
    fiend: false,
    banisher: None,
    p0_out: false,
    raptor: false,
    soul_warden: None,
    opponents_turn: false,
    opponents_bolts: 0,
    p1_sorcerer: false,
    lumberknot: false,
    sink: None,
    p1_bears: false,
    archer: false,
    ichor_rats: false,
    rest_in_peace: false,
    p0_bears: 0,
    first: None,
    wood_elves: None,
    tremors: false,
};

const ANCHOR: Board = Board {
    maggots: 1,
    first: Some("Brain Maggot"),
    ..PLAIN
};

/// Worldgorger Dragon returned by Animate Dead repeats with no player choice, so under
/// Interactive loop detection the game is a draw on every drive.
#[test]
fn worldgorger_animate_dead_is_a_draw_on_every_drive() {
    for drive in EVERY_DRIVE {
        let Some(run) = run(PLAIN, LoopDetectionMode::Interactive, drive, 300) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            run.entries >= 2,
            "{drive:?}: reach: the Dragon entered at least twice, got {}",
            run.entries
        );
    }
}

/// CR 603.3b + CR 732.4: Brain Maggot's forced reveal of P1's only nonland card repeats each
/// cycle with one legal option, and the saved order of Brain Maggot's and Animate Dead's
/// triggers is no decision, since the loop continues under either order; so the loop is a draw.
#[test]
fn worldgorger_with_a_forced_reveal_under_a_saved_order_is_a_draw() {
    for drive in EVERY_DRIVE {
        let Some(run) = run(ANCHOR, LoopDetectionMode::Interactive, drive, 120) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            run.reveals >= 2,
            "{drive:?}: reach: reveals were answered across cycles, got {}",
            run.reveals
        );
        assert_eq!(
            run.prompted("OrderTriggers"),
            1,
            "{drive:?}: reach: the trigger order was asked once and then saved"
        );
        assert!(
            run.entries >= 3,
            "{drive:?}: reach: the saved order applied over several cycles, got {}",
            run.entries
        );
    }
}

/// Loop detection Off keeps the game running on every drive.
#[test]
fn worldgorger_runs_on_with_loop_detection_off() {
    for (board, beats) in [(PLAIN, 300), (ANCHOR, 400)] {
        for drive in EVERY_DRIVE {
            let Some(run) = run(board, LoopDetectionMode::Off, drive, beats) else {
                return;
            };
            assert_eq!(run.terminal, Terminal::BeatsExhausted, "{drive:?}");
            assert!(
                run.entries > run.entries_at_half,
                "{drive:?}: reach: the Dragon keeps entering ({} at half, {} at the end)",
                run.entries_at_half,
                run.entries
            );
            if board.maggots > 0 {
                assert!(
                    run.reveals > run.reveals_at_half,
                    "{drive:?}: reach: the reveals keep coming ({} at half, {} at the end)",
                    run.reveals_at_half,
                    run.reveals
                );
            }
        }
    }
}

/// A position compared against its own sample trivially recurs; a finite reanimation
/// must settle rather than draw.
#[test]
fn a_finite_reanimation_settles_without_a_draw() {
    let board = Board {
        bears: true,
        ..PLAIN
    };
    let Some(run) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 60) else {
        return;
    };
    assert_eq!(run.terminal, Terminal::SettledOnEmptyStack);
    let bears = run
        .runner
        .state()
        .objects
        .get(&run.target)
        .expect("Bears exist");
    assert_eq!(bears.zone, Zone::Battlefield, "reach: the Bears returned");
}

/// CR 104.4b + CR 732.5: "Loops that contain an optional action don't result in a draw." P1
/// can cast Lightning Bolt at any point and nobody is forced to, so P0 is offered a draw without
/// end; Off runs on.
#[test]
fn worldgorger_with_an_optional_action_is_not_a_draw() {
    let board = Board {
        optional: true,
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 300) else {
            return;
        };
        assert_out_offer(&run, &format!("{drive:?}"));
        assert!(
            run.max_ring >= 2,
            "{drive:?}: reach: the loop accumulated a ring, max {}",
            run.max_ring
        );
    }
    for drive in EVERY_DRIVE {
        let Some(run) = run(board, LoopDetectionMode::Off, drive, 40) else {
            return;
        };
        assert_runs_on(&run, &format!("Off {drive:?}"));
    }
}

/// Asserts the run stopped at P0's offer of a draw without end, naming no winner and marking
/// nothing unbounded.
fn assert_out_offer(run: &Run, leg: &str) {
    assert_eq!(
        run.terminal,
        Terminal::Offer {
            predicted_winner: None,
            mandatory: false
        },
        "{leg}"
    );
    let state = run.state();
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            schema,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0, "{leg}");
            assert_eq!(schema.iteration_count, IterationCount::WithoutEnd, "{leg}");
            assert!(certificate.unbounded.is_empty(), "{leg}");
        }
        other => panic!("{leg}: no offer: {other:?}"),
    }
    assert_eq!(
        legal_actions(state),
        vec![DECLARE_WITHOUT_END, GameAction::DeclineShortcut],
        "{leg}"
    );
    assert!(state.unbounded_resources.is_empty(), "{leg}");
}

/// CR 104.4b + CR 603.5: P0 answers Leonin Snarecaster's "may" every cycle beside P1's Bolt, so
/// the period holds an optional action and is neither drawn nor offered.
#[test]
fn a_may_beside_an_out_is_not_offered() {
    let board = Board {
        optional: true,
        ..snarecaster(May::Prompted { accept: true })
    };
    for drive in EVERY_DRIVE {
        let leg = format!("{drive:?}");
        let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 40) else {
            return;
        };
        assert_runs_on(&run, &leg);
        assert!(
            run.prompted("OptionalEffectChoice") >= 2,
            "{leg}: reach: the may was asked across cycles"
        );
    }
}

const MODES: [LoopDetectionMode; 2] = [LoopDetectionMode::Interactive, LoopDetectionMode::Off];

fn snarecaster(may: May) -> Board {
    Board {
        snarecaster: Some(may),
        ..PLAIN
    }
}

/// Asserts the loop ran its beats without being declared while the Dragon kept entering.
fn assert_runs_on(run: &Run, leg: &str) {
    assert_eq!(run.terminal, Terminal::BeatsExhausted, "{leg}");
    assert!(
        run.entries > run.entries_at_half && run.entries_at_half > 0,
        "{leg}: reach: the Dragon keeps entering ({} at half, {} at the end)",
        run.entries_at_half,
        run.entries
    );
}

/// CR 104.4b + CR 603.5: Leonin Snarecaster's "you may tap" is answered every cycle, so the
/// loop holds an optional action and is no draw, whichever way it is answered.
#[test]
fn a_may_answered_every_cycle_is_not_a_draw() {
    for accept in [true, false] {
        for mode in MODES {
            for drive in EVERY_DRIVE {
                let leg = format!("accept={accept} {mode:?} {drive:?}");
                let Some(run) = run(snarecaster(May::Prompted { accept }), mode, drive, 40) else {
                    return;
                };
                assert_runs_on(&run, &leg);
                assert!(
                    run.prompted("OptionalEffectChoice") >= 2,
                    "{leg}: reach: the may was asked across cycles"
                );
            }
        }
    }
}

#[test]
fn a_remembered_may_decline_under_off_is_not_a_draw() {
    remembered_answer_runs_on(false, LoopDetectionMode::Off);
}

#[test]
fn a_remembered_may_accept_under_interactive_is_not_a_draw() {
    remembered_answer_runs_on(true, LoopDetectionMode::Interactive);
}

#[test]
fn a_remembered_may_accept_under_off_is_not_a_draw() {
    remembered_answer_runs_on(true, LoopDetectionMode::Off);
}

/// CR 104.4b + CR 603.5: a "may" answered once and remembered is still answered every cycle,
/// so the loop holds an optional action and is no draw.
fn remembered_answer_runs_on(accept: bool, mode: LoopDetectionMode) {
    remembered_may_runs_on(&[(snarecaster(May::Remembered { accept }), mode, Drive::Pass)]);
}

/// The Interactive remembered-decline legs, one drive per test: each replays its period
/// at every beat.
#[test]
fn a_remembered_may_decline_passing_is_not_a_draw() {
    remembered_decline_runs_on(Drive::Pass);
}

#[test]
fn a_remembered_may_decline_until_stack_empty_is_not_a_draw() {
    remembered_decline_runs_on(Drive::UntilStackEmpty);
}

#[test]
fn a_remembered_may_decline_until_turn_boundary_is_not_a_draw() {
    remembered_decline_runs_on(Drive::UntilTurnBoundary);
}

fn remembered_decline_runs_on(drive: Drive) {
    let decline = snarecaster(May::Remembered { accept: false });
    remembered_may_runs_on(&[(decline, LoopDetectionMode::Interactive, drive)]);
}

fn remembered_may_runs_on(legs: &[(Board, LoopDetectionMode, Drive)]) {
    for &(board, mode, drive) in legs {
        let leg = format!("{:?} {mode:?} {drive:?}", board.snarecaster);
        let Some(run) = run(board, mode, drive, 40) else {
            return;
        };
        assert_runs_on(&run, &leg);
        assert_eq!(
            run.prompted("OptionalEffectChoice"),
            1,
            "{leg}: reach: the may was asked once and then remembered"
        );
    }
}

#[test]
fn a_remembered_optional_prevention_passing_is_not_a_draw() {
    remembered_prevention_runs_on(Drive::Pass);
}

#[test]
fn a_remembered_optional_prevention_until_turn_boundary_is_not_a_draw() {
    remembered_prevention_runs_on(Drive::UntilTurnBoundary);
}

#[test]
fn a_remembered_optional_prevention_until_stack_empty_is_not_a_draw() {
    remembered_prevention_runs_on(Drive::UntilStackEmpty);
}

/// CR 104.4b + CR 616.1e: Battletide Alchemist's prevention of Gibbering Fiend's damage,
/// remembered once, is chosen every cycle, so the loop is no draw.
fn remembered_prevention_runs_on(drive: Drive) {
    let board = Board {
        fiend: true,
        ..PLAIN
    };
    for mode in MODES {
        let leg = format!("{mode:?} {drive:?}");
        let Some(run) = run(board, mode, drive, 30) else {
            return;
        };
        assert_runs_on(&run, &leg);
        assert_eq!(
            run.prompted("ReplacementChoice"),
            1,
            "{leg}: reach: the prevention was asked once and then remembered"
        );
    }
}

/// Asserts that, under each of `picks`, every drive at `board` stops at P0's offer of a draw
/// without end after at least `choices` prompts of `prompt`, whose counted take is `take` (CR
/// 732.2c), and returns the first drive's run.
fn offered_on_every_drive(
    board: Board,
    picks: &[Pick],
    prompt: &str,
    choices: usize,
    take: CountedTake,
) -> Option<Run> {
    let mut first_run = None;
    for &pick in picks {
        let board = Board { pick, ..board };
        for drive in EVERY_DRIVE {
            let leg = format!("{:?} {pick:?} {drive:?}", board.first);
            let run = run(board, LoopDetectionMode::Interactive, drive, 60)?;
            assert_out_offer(&run, &leg);
            assert!(
                run.prompted(prompt) >= choices,
                "{leg}: reach: {prompt} was answered {choices} times, prompts {:?}",
                run.prompts
            );
            match take {
                CountedTake::Unperformable(_) => assert_no_counted_take(run.state(), take, &leg),
                CountedTake::Performs => assert_eq!(counted_take(run.state()), take, "{leg}"),
            }
            first_run.get_or_insert(run);
        }
    }
    first_run
}

const TWO_TARGETS: Board = Board {
    banisher: Some(2),
    ..PLAIN
};

const TREMORS_TWO_TARGETS: Board = Board {
    tremors: true,
    ..TWO_TARGETS
};

/// CR 601.2c + CR 732.2a: Banisher Priest's trigger chooses between two creatures each cycle,
/// and the loop continues whichever is exiled, so P0 is offered a draw without end, and every
/// player accepting it draws the game; Off runs on.
#[test]
fn a_target_chosen_every_cycle_is_offered() {
    let Some(offered) = offered_on_every_drive(
        TWO_TARGETS,
        &[Pick::First, Pick::Alternate],
        "TriggerTargetSelection",
        2,
        CountedTake::Unperformable(UnperformableCause::UncontainedChoice),
    ) else {
        return;
    };
    offer_accepted_is_a_draw(offered);
    for drive in EVERY_DRIVE {
        let Some(run) = run(TWO_TARGETS, LoopDetectionMode::Off, drive, 40) else {
            return;
        };
        assert_runs_on(&run, &format!("Off {drive:?}"));
    }
}

/// CR 601.2c + CR 732.2a: with Animate Dead's trigger first, the first cycle's one target choice
/// is the period read, and the loop continues whichever creature Banisher Priest exiles, so P0 is
/// offered a draw without end.
#[test]
fn a_target_chosen_every_cycle_after_animate_deads_trigger_is_offered() {
    let board = Board {
        first: Some("Animate Dead"),
        ..TWO_TARGETS
    };
    offered_on_every_drive(
        board,
        &[Pick::First, Pick::Alternate],
        "TriggerTargetSelection",
        1,
        CountedTake::Unperformable(UnperformableCause::UncontainedChoice),
    );
}

/// Declares the offer `offered` stopped at, and asserts every player accepting it draws the game.
fn offer_accepted_is_a_draw(mut offered: Run) {
    assert_out_offer(&offered, "accepted");
    offered.runner.act(DECLARE_WITHOUT_END).expect("declare");
    offered
        .runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        offered.state().waiting_for,
        WaitingFor::GameOver { winner: None }
    );
}

const TWO_CARDS: Board = Board {
    maggots: 1,
    murder: true,
    ..PLAIN
};

/// CR 608.2d + CR 732.2a: Brain Maggot's trigger makes P0 choose between P1's Lightning Bolt and
/// Murder each cycle, and the loop continues whichever is exiled, so P0 is offered a draw without
/// end, and every player accepting it draws the game; Off runs on.
#[test]
fn a_card_chosen_every_cycle_is_offered() {
    let Some(offered) = offered_on_every_drive(
        TWO_CARDS,
        &[Pick::First],
        "RevealChoice",
        2,
        CountedTake::Unperformable(UnperformableCause::UncontainedChoice),
    ) else {
        return;
    };
    offer_accepted_is_a_draw(offered);
    for drive in EVERY_DRIVE {
        let Some(run) = run(TWO_CARDS, LoopDetectionMode::Off, drive, 40) else {
            return;
        };
        assert_runs_on(&run, &format!("Off {drive:?}"));
    }
}

/// CR 608.2d + CR 732.2a: P0 choosing P1's Lightning Bolt and Murder in turn at Brain Maggot's
/// trigger each cycle is offered a draw without end, since the loop continues whichever is exiled.
#[test]
fn a_card_chosen_in_turn_every_cycle_is_offered() {
    offered_on_every_drive(
        TWO_CARDS,
        &[Pick::Alternate],
        "RevealChoice",
        2,
        CountedTake::Unperformable(UnperformableCause::UncontainedChoice),
    );
}

/// CR 104.4b + CR 601.2c: Oblivion Ring's trigger can target P0's own Animate Dead, which ends the
/// loop, so choosing its target each cycle holds an optional action even when P0 always exiles
/// P1's Grizzly Bears, and the loop is neither drawn nor offered.
#[test]
fn a_target_choice_that_can_end_the_loop_is_not_offered() {
    let board = Board {
        oring: true,
        p1_bears: true,
        pick: Pick::Named("Grizzly Bears"),
        ..PLAIN
    };
    for mode in MODES {
        for drive in EVERY_DRIVE {
            let leg = format!("{mode:?} {drive:?}");
            let Some(run) = run(board, mode, drive, 40) else {
                return;
            };
            assert_runs_on(&run, &leg);
            assert!(
                run.prompted("TriggerTargetSelection") >= 2,
                "{leg}: reach: a target was chosen across cycles"
            );
        }
    }
}

/// CR 104.4b + CR 601.2c: Banisher Priest's trigger chooses among three creatures each cycle;
/// every choice continues the loop, but its choices are past the every-option proof's budget, so
/// the loop is not offered.
#[test]
fn a_choice_tree_past_the_budget_is_not_offered() {
    let board = Board {
        banisher: Some(3),
        ..PLAIN
    };
    let Some(run) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 90) else {
        return;
    };
    assert_runs_on(&run, "Pass");
    assert!(
        run.prompted("TriggerTargetSelection") >= 3,
        "reach: a target was chosen across cycles"
    );
    assert!(
        run.replays.iter().any(|&(_, _, replays)| replays > 1),
        "reach: the every-option proof expanded the choices"
    );
}

/// CR 603.3b + CR 608.2d + CR 732.4: with Animate Dead's trigger resolving first, the period's
/// only decision is the saved trigger order, and under the other order Brain Maggot's choice
/// between two revealed cards continues the loop whichever is chosen, so the loop is a draw.
#[test]
fn worldgorger_choosing_between_two_revealed_cards_under_a_saved_order_is_a_draw() {
    let board = Board {
        first: Some("Animate Dead"),
        ..TWO_CARDS
    };
    for drive in [Drive::Pass, Drive::UntilTurnBoundary] {
        let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 150) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            run.prompted("OrderTriggers") >= 1,
            "{drive:?}: reach: the triggers were ordered"
        );
    }
}

/// CR 603.3b + CR 732.5: Soul Warden's trigger beside the Dragon's is an ordering point under
/// which either order continues, so with P1's Bolt outside the loop P0 is offered a draw without
/// end, and every player accepting it draws the game; Off runs on.
#[test]
fn a_loop_whose_every_trigger_order_continues_beside_an_out_is_offered() {
    let board = Board {
        optional: true,
        soul_warden: Some(P0),
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let leg = format!("{drive:?}");
        let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 200) else {
            return;
        };
        assert_out_offer(&run, &leg);
        assert!(
            run.prompted("OrderTriggers") >= 1,
            "{leg}: reach: the triggers were ordered"
        );
    }
    let Some(mut offered) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 200) else {
        return;
    };
    offered.runner.act(DECLARE_WITHOUT_END).expect("declare");
    offered
        .runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        offered.state().waiting_for,
        WaitingFor::GameOver { winner: None }
    );
    for drive in EVERY_DRIVE {
        let Some(run) = run(board, LoopDetectionMode::Off, drive, 40) else {
            return;
        };
        assert_runs_on(&run, &format!("Off {drive:?}"));
    }
}

/// CR 104.4b + CR 732.5: P0 can cast Lightning Bolt whenever its Mountain is back on the
/// battlefield, so the loop holds an out and is no draw, even judged from a position where
/// the Mountain is exiled: Interactive offers P0 a draw without end, Off runs on.
#[test]
fn an_out_held_within_the_period_is_not_a_draw() {
    let p0_out = Board {
        p0_out: true,
        ..PLAIN
    };
    let p1_out = Board {
        optional: true,
        ..PLAIN
    };
    for (board, modes) in [
        (p0_out, &MODES[..]),
        (p1_out, &[LoopDetectionMode::Off][..]),
    ] {
        for &mode in modes {
            for drive in EVERY_DRIVE {
                let leg = format!("p0_out={} {mode:?} {drive:?}", board.p0_out);
                let Some(run) = run(board, mode, drive, 40) else {
                    return;
                };
                if mode == LoopDetectionMode::Interactive {
                    assert_out_offer(&run, &leg);
                } else {
                    assert_runs_on(&run, &leg);
                }
                if board.p0_out {
                    assert!(
                        run.out_beats >= 1,
                        "{leg}: reach: P0's Mountain stood untapped at some priority"
                    );
                }
            }
        }
    }
}

/// Off, with full yields one action drives many cycles, and the auto-pass backstop declares
/// none of these boards.
#[test]
fn full_yield_drives_run_on_with_loop_detection_off() {
    let fiend = Board {
        fiend: true,
        ..PLAIN
    };
    for (leg, board, beats, remembered) in [
        ("plain", PLAIN, 7, None),
        (
            "remembered decline",
            snarecaster(May::Remembered { accept: false }),
            11,
            Some("OptionalEffectChoice"),
        ),
        (
            "remembered accept",
            snarecaster(May::Remembered { accept: true }),
            11,
            Some("OptionalEffectChoice"),
        ),
        (
            "remembered prevention",
            fiend,
            11,
            Some("ReplacementChoice"),
        ),
    ] {
        let Some(run) = run(board, LoopDetectionMode::Off, Drive::FullYields, beats) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::BeatsExhausted, "{leg}");
        assert!(
            run.max_entries_per_action >= 16,
            "{leg}: reach: one action returned the Dragon many times, got {}",
            run.max_entries_per_action
        );
        if let Some(prompt) = remembered {
            assert_eq!(
                run.prompted(prompt),
                1,
                "{leg}: reach: one remembered answer"
            );
        }
    }
}

/// Under Interactive, the capped auto-pass drive over a mandatory loop still draws.
#[test]
fn a_capped_auto_pass_drive_over_a_mandatory_loop_draws() {
    let Some((terminal, _, _)) = full_yield_drive_returns(PLAIN, 12, capped_reading) else {
        return;
    };
    assert_eq!(
        terminal,
        Terminal::GameOver(None),
        "control: the mandatory loop draws"
    );
}

/// An auto-pass drive that reaches its iteration cap hands control back, so under
/// Interactive the action returns even when the loop it ran is not declared.
#[test]
fn a_capped_auto_pass_action_returns_after_a_remembered_accept() {
    capped_drive_runs_on(snarecaster(May::Remembered { accept: true }));
}

#[test]
fn a_capped_auto_pass_action_returns_after_a_remembered_decline() {
    capped_drive_runs_on(snarecaster(May::Remembered { accept: false }));
}

fn capped_drive_runs_on(board: Board) {
    let leg = format!("{:?}", board.snarecaster);
    let Some((terminal, max_entries_per_action, prompts)) =
        full_yield_drive_returns(board, 12, capped_reading)
    else {
        return;
    };
    assert_eq!(terminal, Terminal::BeatsExhausted, "{leg}");
    assert!(
        max_entries_per_action >= 16,
        "{leg}: reach: one action reached the iteration cap, got {max_entries_per_action}"
    );
    assert_eq!(prompts, 1, "{leg}: reach: one remembered answer");
}

fn capped_reading(run: Run) -> (Terminal, usize, usize) {
    let prompts = run.prompted("OptionalEffectChoice");
    (run.terminal, run.max_entries_per_action, prompts)
}

/// Runs an Interactive full-yield drive of `beats` and reads the run.
fn full_yield_drive_returns<T>(board: Board, beats: usize, read: fn(Run) -> T) -> Option<T> {
    run(
        board,
        LoopDetectionMode::Interactive,
        Drive::FullYields,
        beats,
    )
    .map(read)
}

/// CR 117.4 + CR 732.2a: a period confirmed on a frame where its holder stands on an auto-pass
/// is replayed one recorded pass at a time, so each triggered ability the replay resolves takes
/// a pass from both players, each its own action.
#[test]
fn a_standing_auto_pass_does_not_pass_inside_a_confirm_replay() {
    let before = play_trace_counters();
    let Some(run) = run(
        snarecaster(May::Remembered { accept: false }),
        LoopDetectionMode::Interactive,
        Drive::UntilTurnBoundary,
        12,
    ) else {
        return;
    };
    let counted = play_trace_counters().since(before);
    assert_eq!(run.terminal, Terminal::BeatsExhausted);
    assert!(
        counted.confirm_drives > 0 && counted.resolution_hooks_in_probe > 0,
        "reach: a replay resolved triggered abilities ({counted:?})"
    );
    assert!(
        2 * counted.resolution_hooks_in_probe <= counted.probe_entries,
        "a replayed action passed for its holder as well ({counted:?})"
    );
}

const RAPTOR: Board = Board {
    raptor: true,
    ..PLAIN
};

/// CR 104.4b + CR 732.4: Marauding Raptor deals 2 damage to each Polyraptor copy as it enters,
/// and the copy's enrage makes another; nothing in the loop is optional, so it is a draw on every
/// drive. Off declares nothing, and the battlefield keeps growing.
#[test]
fn a_mandatory_token_copy_loop_is_a_draw_on_every_drive() {
    for drive in [
        Drive::Pass,
        Drive::UntilTurnBoundary,
        Drive::UntilStackEmpty,
        Drive::P0UntilStackEmpty,
    ] {
        let Some(run) = run(RAPTOR, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            run.on_battlefield("Polyraptor").len() >= 2,
            "{drive:?}: reach: a copy entered"
        );
    }
    let Some((terminal, polyraptors)) = full_yield_drive_returns(RAPTOR, 6, |run| {
        let polyraptors = run.on_battlefield("Polyraptor").len();
        (run.terminal, polyraptors)
    }) else {
        return;
    };
    assert_eq!(terminal, Terminal::GameOver(None), "FullYields");
    assert!(polyraptors >= 2, "FullYields: reach: a copy entered");
    for drive in EVERY_DRIVE {
        let Some(run) = run(RAPTOR, LoopDetectionMode::Off, drive, 60) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::BeatsExhausted, "Off {drive:?}");
        assert!(
            run.on_battlefield("Polyraptor").len() >= 4,
            "Off {drive:?}: reach: the copies kept coming"
        );
    }
}

const LUMBERKNOT: Board = Board {
    lumberknot: true,
    ..PLAIN
};

/// CR 104.4b + CR 732.4: Lumberknot grows a +1/+1 counter at each Dragon death, and nothing in
/// the loop reads it, so the loop is still a draw.
#[test]
fn monotone_counter_growth_nothing_reads_is_a_draw() {
    for drive in EVERY_DRIVE {
        let Some(run) = run(LUMBERKNOT, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            run.counters_on("Lumberknot", CounterType::Plus1Plus1) >= 1,
            "{drive:?}: reach: Lumberknot grew"
        );
        let Some(off) = run_off(LUMBERKNOT, drive, 60) else {
            return;
        };
        assert!(
            off.counters_on("Lumberknot", CounterType::Plus1Plus1) >= 2,
            "Off {drive:?}: reach: Lumberknot kept growing"
        );
    }
}

/// Runs `board` with loop detection Off and asserts it ran its beats with the Dragon still
/// entering.
fn run_off(board: Board, drive: Drive, beats: usize) -> Option<Run> {
    let run = run(board, LoopDetectionMode::Off, drive, beats)?;
    assert_runs_on(&run, &format!("Off {drive:?}"));
    Some(run)
}

const MINUS_SINK: &str = "Whenever a creature dies, put a -1/-1 counter on this creature.";
const SPREAD_SINK: &str =
    "Whenever a creature dies, put a +1/+1 counter on each creature you control.";

/// CR 704.5f + CR 704.5q: a loop growing -1/-1 counters toward a creature's death is not a draw,
/// and one whose +1/+1 counters annihilate a creature's -1/-1 counters is a draw only once that
/// annihilation has run its course.
#[test]
fn counter_growth_toward_a_state_based_action_is_not_a_draw() {
    let shrinking = Board {
        sink: Some((MINUS_SINK, 0)),
        ..PLAIN
    };
    let annihilating = Board {
        sink: Some((SPREAD_SINK, 4)),
        p1_bears: true,
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let Some(shrunk) = run(shrinking, LoopDetectionMode::Interactive, drive, 120) else {
            return;
        };
        assert_eq!(shrunk.terminal, Terminal::BeatsExhausted, "{drive:?}");
        assert!(
            shrunk.counters_on("Test Counter Sink", CounterType::Minus1Minus1) >= 10,
            "{drive:?}: reach: the -1/-1 counters grew"
        );

        let Some(annihilated) = run(annihilating, LoopDetectionMode::Interactive, drive, 120)
        else {
            return;
        };
        assert_eq!(annihilated.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            annihilated.counters_on("Test Counter Sink", CounterType::Minus1Minus1) <= 1,
            "{drive:?}: declared before the annihilation ran its course"
        );
        assert!(
            annihilated.counters_on("Grizzly Bears", CounterType::Plus1Plus1) >= 1,
            "{drive:?}: reach: the Bears grew"
        );

        for board in [shrinking, annihilating] {
            if run_off(board, drive, 60).is_none() {
                return;
            }
        }
    }
}

/// CR 704.5c: a loop giving each player a poison counter at each Dragon death is not declared; it
/// ends when both players reach ten poison counters.
#[test]
fn a_poison_axis_loop_is_not_declared() {
    let board = Board {
        sink: Some((
            "Whenever a creature dies, each player gets a poison counter.",
            0,
        )),
        ..PLAIN
    };
    for mode in MODES {
        for drive in EVERY_DRIVE {
            let Some(run) = run(board, mode, drive, 120) else {
                return;
            };
            assert_eq!(run.terminal, Terminal::GameOver(None), "{mode:?} {drive:?}");
            assert!(
                run.poison(P0) >= 10 && run.poison(P1) >= 10,
                "{mode:?} {drive:?}: the game ended at ten poison counters"
            );
        }
    }
}

/// CR 104.4b + CR 603.3b + CR 732.4: with Soul Warden, P0 orders Raptor's and Warden's triggers
/// each cycle, and the copies keep coming whichever resolves first, so the order is no decision
/// and the growth loop is a draw.
#[test]
fn a_mandatory_growth_loop_whose_every_trigger_order_continues_is_a_draw() {
    for first in ["Marauding Raptor", "Soul Warden"] {
        let board = Board {
            soul_warden: Some(P0),
            first: Some(first),
            ..RAPTOR
        };
        for drive in EVERY_DRIVE {
            let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
                return;
            };
            assert_eq!(run.terminal, Terminal::GameOver(None), "{first} {drive:?}");
            assert!(
                run.prompted("OrderTriggers") >= 1,
                "{first} {drive:?}: reach: the triggers were ordered"
            );
            assert!(
                run.on_battlefield("Polyraptor").len() >= 2,
                "{first} {drive:?}: reach: the copies kept coming"
            );
        }
        let Some(off) = run(board, LoopDetectionMode::Off, Drive::Pass, 60) else {
            return;
        };
        assert_eq!(off.terminal, Terminal::BeatsExhausted, "Off {first}");
        assert!(
            off.on_battlefield("Polyraptor").len() >= 3,
            "Off {first}: reach: the copies kept coming"
        );
    }
}

/// CR 608.1 + CR 732.2a: a window whose stack shrinks and then returns to its depth keeps its
/// samples across the shrinking beat.
#[test]
fn a_shrinking_beat_inside_a_returning_window_keeps_the_ring() {
    let Some(run) = run(LUMBERKNOT, LoopDetectionMode::Interactive, Drive::Pass, 60) else {
        return;
    };
    assert_eq!(run.terminal, Terminal::GameOver(None));
    assert!(run.max_ring > 4, "max ring {}", run.max_ring);
}

/// CR 732.2b: Poison-Tip Archer drains P0 at each Dragon death. With nothing P0 can do, P1 wins
/// before the drain ends; with a Bolt P0 can cast while its Mountain is back, the drain is
/// offered, and a counted take of it drives that many cycles (CR 732.2c).
#[test]
fn a_drain_with_an_out_outside_the_loop_is_offered_not_won() {
    let drain = Board {
        archer: true,
        ..PLAIN
    };
    let with_out = Board {
        p0_out: true,
        ..drain
    };
    for drive in EVERY_DRIVE {
        let Some(won) = run(drain, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert_eq!(won.terminal, Terminal::GameOver(Some(P1)), "{drive:?}");
        assert!(won.life(P0) > 0, "{drive:?}: won before the drain ended");

        let Some(offered) = run(with_out, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert_eq!(
            offered.terminal,
            Terminal::Offer {
                predicted_winner: Some(P1),
                mandatory: false
            },
            "{drive:?}"
        );
        let leg = format!("{drive:?}");
        let state = offered.state();
        assert_eq!(counted_take(state), CountedTake::Performs, "{leg}");
        let (window, _) = admitted(state, P0, declare(IterationCount::Fixed(3)), &leg);
        assert!(responds(&window, P1), "{leg}: {:?}", window.waiting_for);
        let (one_state, one) = admitted(&window, P1, shorten_at(1), &leg);
        let cycle = battlefield_entries(&one);
        let lost = state.players[0].life - one_state.players[0].life;
        assert!(
            cycle > 0 && lost > 0,
            "{leg}: reach: a cycle enters permanents and drains P0"
        );
        let (accepted_state, accepted) = admitted(
            &window,
            P1,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
            &leg,
        );
        assert_eq!(repetitions(&accepted), 3, "{leg}");
        assert_eq!(
            state.players[0].life - accepted_state.players[0].life,
            3 * lost,
            "{leg}"
        );

        for board in [drain, with_out] {
            let Some(off) = run_off(board, drive, 60) else {
                return;
            };
            assert!(off.life(P0) < 20, "Off {drive:?}: reach: P0 was drained");
        }
    }
}

/// CR 603.3d + CR 601.2c: Banisher Priest's trigger chooses between two Grizzly Bears each cycle,
/// a choice no declaration contains, while Impact Tremors drains P1 (CR 120.3a), so P0's winning
/// loop is played out to P1's loss (CR 704.5a), as Off plays it.
#[test]
fn a_winning_loop_holding_a_target_choice_passing_is_played_out() {
    winning_choice_loop_is_played_out(TREMORS_TWO_TARGETS, Drive::Pass);
}

#[test]
fn a_winning_loop_holding_a_target_choice_until_turn_boundary_is_played_out() {
    winning_choice_loop_is_played_out(TREMORS_TWO_TARGETS, Drive::UntilTurnBoundary);
}

#[test]
fn a_winning_loop_holding_a_target_choice_until_stack_empty_is_played_out() {
    winning_choice_loop_is_played_out(TREMORS_TWO_TARGETS, Drive::UntilStackEmpty);
}

/// CR 732.5: P0's Bolt is an out beside the target choice, and the winning loop is still played
/// out.
#[test]
fn a_winning_loop_holding_a_target_choice_beside_an_out_is_played_out() {
    winning_choice_loop_is_played_out(
        Board {
            p0_out: true,
            ..TREMORS_TWO_TARGETS
        },
        Drive::Pass,
    );
}

/// Asserts `drive` at `board` ends, under Off and under Interactive, in P0's win at P1's 0 life,
/// with every target choice made in play and no action ordering no triggers running more than
/// one confirm drive.
fn winning_choice_loop_is_played_out(board: Board, drive: Drive) {
    let leg = format!("p0_out={} {drive:?}", board.p0_out);
    let Some(off) = run(board, LoopDetectionMode::Off, drive, 400) else {
        return;
    };
    assert_eq!(off.terminal, Terminal::GameOver(Some(P0)), "Off {leg}");
    assert_eq!(off.life(P1), 0, "Off {leg}");
    let Some(played) = run(board, LoopDetectionMode::Interactive, drive, 400) else {
        return;
    };
    assert_eq!(played.terminal, Terminal::GameOver(Some(P0)), "{leg}");
    assert_eq!(played.life(P1), 0, "{leg}");
    assert_eq!(
        played.prompted("TriggerTargetSelection"),
        off.prompted("TriggerTargetSelection"),
        "{leg}: reach: every target choice was made in play"
    );
    assert!(
        played
            .replays
            .iter()
            .map(|&(_, _, drives)| drives)
            .sum::<u64>()
            > 0,
        "{leg}: reach: the confirm drives ran"
    );
    assert!(
        played
            .replays
            .iter()
            .filter(|&&(_, ordered, _)| !ordered)
            .all(|&(_, _, drives)| drives <= 1),
        "{leg}: an action ordering no triggers ran at most one confirm drive"
    );
}

/// CR 603.3b + CR 704.5a: against one Grizzly Bears, Banisher Priest's trigger has one target, so
/// the period holds only the trigger order, and P0 wins outright before the drain ends; Off plays
/// it to P1's 0 life.
#[test]
fn a_forced_winning_loop_passing_is_won_outright() {
    forced_winning_loop_is_won(Drive::Pass);
}

#[test]
fn a_forced_winning_loop_until_turn_boundary_is_won_outright() {
    forced_winning_loop_is_won(Drive::UntilTurnBoundary);
}

#[test]
fn a_forced_winning_loop_until_stack_empty_is_won_outright() {
    forced_winning_loop_is_won(Drive::UntilStackEmpty);
}

fn forced_winning_loop_is_won(drive: Drive) {
    let board = Board {
        tremors: true,
        banisher: Some(1),
        ..PLAIN
    };
    let leg = format!("{drive:?}");
    let Some(won) = run(board, LoopDetectionMode::Interactive, drive, 250) else {
        return;
    };
    assert!(
        won.prompted("OrderTriggers") >= 1,
        "{leg}: reach: the triggers were ordered"
    );
    assert_eq!(won.terminal, Terminal::GameOver(Some(P0)), "{leg}");
    assert!(won.life(P1) > 0, "{leg}: won before the drain ended");
    let Some(off) = run(board, LoopDetectionMode::Off, drive, 250) else {
        return;
    };
    assert_eq!(off.terminal, Terminal::GameOver(Some(P0)), "Off {leg}");
    assert_eq!(off.life(P1), 0, "Off {leg}");
}

/// CR 732.2c: a restored winning offer whose take refuses one repetition refuses P0's counted and
/// until-lethal declarations with the board as it was, and the AI's only move is to decline.
#[test]
fn a_restored_winning_offer_over_a_choice_refuses_every_counted_declaration() {
    let choice = UnperformableCause::UncontainedChoice;
    let board = Board {
        archer: true,
        p0_out: true,
        ..PLAIN
    };
    let leg = "restored";
    let Some(offered) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 60) else {
        return;
    };
    assert_eq!(
        offered.terminal,
        Terminal::Offer {
            predicted_winner: Some(P1),
            mandatory: false
        },
        "{leg}"
    );
    assert_eq!(
        legal_actions(offered.state()),
        vec![
            declare(IterationCount::UntilLethal),
            GameAction::DeclineShortcut
        ],
        "{leg}: reach: a performing offer takes until lethal"
    );
    let mut carried = offered.state().clone();
    let WaitingFor::LoopShortcut {
        counted_take: take, ..
    } = &mut carried.waiting_for
    else {
        panic!("{leg}: {:?}", carried.waiting_for);
    };
    *take = CountedTake::Unperformable(choice);
    let restored = serde_json::from_value::<PersistedGameState>(
        serde_json::to_value(&carried).expect("the offer encodes"),
    )
    .expect("the offer decodes")
    .into_game_state()
    .expect("the offer restores");
    assert_eq!(
        counted_take(&restored),
        CountedTake::Unperformable(choice),
        "{leg}: reach"
    );
    for count in [IterationCount::UntilLethal, IterationCount::Fixed(3)] {
        assert_refused(
            &restored,
            P0,
            declare(count.clone()),
            choice.reason(),
            &format!("{leg} {count:?}"),
        );
    }
    let (declined, _) = admitted(&restored, P0, GameAction::DeclineShortcut, leg);
    assert!(
        priority_to(&declined, P0),
        "{leg}: {:?}",
        declined.waiting_for
    );
    assert_eq!(
        legal_actions(&restored),
        vec![GameAction::DeclineShortcut],
        "{leg}"
    );
    run_off(board, Drive::Pass, 60);
}

/// CR 117.3d + CR 732.4: the mandatory period reads the same whichever player holds priority,
/// including a frame where P1's standing pass is paused by its own meaningful action; P1's
/// untapped Prodigal Sorcerer brings an offer P0 declines (CR 732.5).
#[test]
fn a_mandatory_token_copy_loop_reads_the_same_on_either_holder() {
    let board = Board {
        p1_sorcerer: true,
        ..RAPTOR
    };
    let mut readings: Vec<(PlayerId, bool, bool)> = Vec::new();
    let mut observe = |state: &GameState| {
        if let WaitingFor::Priority { player } = state.waiting_for {
            if let Some(holds) = mandatory_period_holds_for_tests(state) {
                readings.push((player, state.stack_resolution_session.is_some(), holds));
            }
        }
    };
    let Some(Run { mut runner, .. }) = run_observed(
        board,
        LoopDetectionMode::Interactive,
        Drive::UntilStackEmpty,
        30,
        &mut observe,
    ) else {
        return;
    };
    let mut declined = 0;
    for _ in 0..30 {
        let state = runner.state();
        observe(state);
        let action = match &state.waiting_for {
            WaitingFor::LoopShortcut { .. } => {
                declined += 1;
                GameAction::DeclineShortcut
            }
            WaitingFor::Priority { .. } => until_stack_empty(),
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("a legal drive action");
    }
    assert!(declined >= 1, "reach: P0 was offered the loop");
    assert!(
        readings
            .iter()
            .any(|&(holder, session, _)| holder == P1 && session),
        "reach: a P1 frame under a paused standing pass: {readings:?}"
    );
    assert!(
        readings.iter().any(|&(holder, _, _)| holder == P0),
        "reach: a P0 frame: {readings:?}"
    );
    assert!(readings.iter().all(|&(_, _, holds)| holds), "{readings:?}");
}

/// The mandatory period is replayed once per span start: P0's passes leave the same span on top,
/// so only P1's passes, which bring a new one, replay it.
#[test]
fn a_mandatory_period_is_replayed_once_per_span_start() {
    let board = Board {
        sink: Some((MINUS_SINK, 0)),
        ..PLAIN
    };
    let Some(run) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 60) else {
        return;
    };
    assert_eq!(run.terminal, Terminal::BeatsExhausted);
    let replays_by = |seat: PlayerId| -> u64 {
        run.replays
            .iter()
            .filter(|&&(acting, _, _)| acting == Some(seat))
            .map(|&(_, _, replays)| replays)
            .sum()
    };
    assert!(
        replays_by(P1) >= 1,
        "reach: P1's passes replayed the period"
    );
    assert_eq!(replays_by(P0), 0);
}

/// Each of a source's triggered abilities below the top of the stack.
fn buried_triggers(state: &GameState, source: &str) -> usize {
    state
        .stack
        .iter()
        .rev()
        .skip(1)
        .filter(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_name, .. } if source_name == source)
        })
        .count()
}

/// CR 603.3b + CR 405.5 + CR 732.4: on P1's turn, P1's Soul Warden trigger goes on the stack
/// before P0's trigger of the same event, so each cycle leaves P1's triggers beneath the loop. No
/// player chooses anything, so the loop is a draw on every drive; with loop detection Off it runs
/// on.
#[test]
fn a_loop_burying_its_opponents_triggers_on_the_opponents_turn_is_a_draw() {
    let board = Board {
        soul_warden: Some(P1),
        opponents_turn: true,
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let mut buried = 0;
        let Some(run) = run_observed(board, LoopDetectionMode::Interactive, drive, 150, |state| {
            buried = buried.max(buried_triggers(state, "Soul Warden"));
        }) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert_eq!(
            run.prompted("OrderTriggers"),
            0,
            "{drive:?}: reach: no player ordered triggers"
        );
        assert!(
            buried >= 2 && run.entries >= 1,
            "{drive:?}: reach: the Dragon re-entered ({}) over buried Soul Warden triggers ({buried})",
            run.entries
        );
    }
    let mut seen = Vec::new();
    let Some(off) = run_observed(board, LoopDetectionMode::Off, Drive::Pass, 150, |state| {
        seen.push(buried_triggers(state, "Soul Warden"));
    }) else {
        return;
    };
    assert_eq!(off.terminal, Terminal::BeatsExhausted);
    assert!(
        seen[seen.len() / 2] < seen[seen.len() - 1],
        "reach: the buried Soul Warden triggers keep growing: {seen:?}"
    );
}

/// Each spell named `name` on the stack below its top object.
fn buried_spells(state: &GameState, name: &str) -> usize {
    state
        .stack
        .iter()
        .rev()
        .skip(1)
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::Spell { .. })
                && state
                    .objects
                    .get(&entry.id)
                    .is_some_and(|object| object.name == name)
        })
        .count()
}

/// CR 405.5 + CR 608.1 + CR 732.4: spells P1 cast before the loop began stay beneath it and never
/// resolve, so the loop is still a draw on every drive.
#[test]
fn a_loop_above_its_opponents_spells_is_a_draw() {
    let board = Board {
        soul_warden: Some(P1),
        opponents_turn: true,
        opponents_bolts: 2,
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let mut bolts = 0;
        let Some(run) = run_observed(board, LoopDetectionMode::Interactive, drive, 150, |state| {
            bolts = bolts.max(buried_spells(state, "Lightning Bolt"));
        }) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert_eq!(
            run.runner.state().players[0].life,
            20,
            "{drive:?}: no Lightning Bolt resolved"
        );
        assert!(
            bolts >= 2 && run.entries >= 1,
            "{drive:?}: reach: the Dragon re-entered ({}) over buried Lightning Bolts ({bolts})",
            run.entries
        );
    }
}

const BURIED_MAGGOT: Board = Board {
    first: Some("Animate Dead"),
    ..ANCHOR
};

/// CR 405.5 + CR 603.3b + CR 732.4: with Animate Dead's trigger resolving first, the Dragon is
/// back before Brain Maggot's trigger resolves, so each cycle leaves one more Brain Maggot trigger
/// beneath the loop. The loop continues under either order and the reveal has one option, so it
/// is a draw on every drive; Off keeps burying.
#[test]
fn worldgorger_burying_brain_maggot_triggers_is_a_draw_on_every_drive() {
    for drive in EVERY_DRIVE {
        let mut buried = 0;
        let Some(run) = run_observed(
            BURIED_MAGGOT,
            LoopDetectionMode::Interactive,
            drive,
            150,
            |state| buried = buried.max(buried_triggers(state, "Brain Maggot")),
        ) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            buried >= 1,
            "{drive:?}: reach: a Brain Maggot trigger was buried"
        );
    }
    let mut depths = Vec::new();
    let Some(off) = run_observed(
        BURIED_MAGGOT,
        LoopDetectionMode::Off,
        Drive::Pass,
        150,
        |state| depths.push(buried_triggers(state, "Brain Maggot")),
    ) else {
        return;
    };
    assert_eq!(off.terminal, Terminal::BeatsExhausted);
    assert!(
        depths[depths.len() - 1] > depths[depths.len() / 2],
        "Off: reach: the buried triggers keep growing"
    );
}

#[test]
fn banisher_priest_against_one_creature_passing_is_a_draw_in_either_order() {
    banisher_priest_against_one_creature_is_a_draw(Drive::Pass);
}

#[test]
fn banisher_priest_against_one_creature_until_turn_boundary_is_a_draw_in_either_order() {
    banisher_priest_against_one_creature_is_a_draw(Drive::UntilTurnBoundary);
}

#[test]
fn banisher_priest_against_one_creature_until_stack_empty_is_a_draw_in_either_order() {
    banisher_priest_against_one_creature_is_a_draw(Drive::UntilStackEmpty);
}

/// CR 603.3b + CR 732.4: Banisher Priest's trigger against P1's only creature has one legal
/// target, and the loop continues whichever of its trigger and Animate Dead's resolves first,
/// so it is a draw in either order; Off, passing, runs on.
fn banisher_priest_against_one_creature_is_a_draw(drive: Drive) {
    for first in ["Banisher Priest", "Animate Dead"] {
        let board = Board {
            banisher: Some(1),
            first: Some(first),
            ..PLAIN
        };
        let Some(played) = run(board, LoopDetectionMode::Interactive, drive, 150) else {
            return;
        };
        assert_eq!(
            played.terminal,
            Terminal::GameOver(None),
            "{first} {drive:?}"
        );
        assert!(
            played.prompted("OrderTriggers") >= 1,
            "{first} {drive:?}: reach: the triggers were ordered"
        );
        if drive == Drive::Pass {
            let Some(off) = run(board, LoopDetectionMode::Off, Drive::Pass, 150) else {
                return;
            };
            assert_runs_on(&off, &format!("Off {first}"));
        }
    }
}

/// CR 104.4b + CR 603.3b: with Rest in Peace, ordering its trigger to resolve first exiles the
/// Dragon from the graveyard before Animate Dead's trigger returns it, ending the loop, so the
/// loop that orders Animate Dead's first holds an optional action and is no draw.
#[test]
fn an_order_that_could_end_the_loop_is_not_a_draw() {
    let board = |first| Board {
        rest_in_peace: true,
        first: Some(first),
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let Some(continued) = run(
            board("Animate Dead"),
            LoopDetectionMode::Interactive,
            drive,
            60,
        ) else {
            return;
        };
        assert_runs_on(&continued, &format!("{drive:?}"));
        assert!(
            continued.prompted("OrderTriggers") >= 2,
            "{drive:?}: reach: the order was asked each cycle"
        );
        let Some(ended) = run(
            board("Rest in Peace"),
            LoopDetectionMode::Interactive,
            drive,
            120,
        ) else {
            return;
        };
        assert_eq!(ended.terminal, Terminal::SettledOnEmptyStack, "{drive:?}");
    }
}

/// The every-order proof is replayed once per span start: P0's passes leave the span its trigger
/// order began on top, so only the actions that begin one replay the orders.
#[test]
fn the_every_order_proof_is_replayed_once_per_span_start() {
    let board = Board {
        rest_in_peace: true,
        first: Some("Animate Dead"),
        ..PLAIN
    };
    let Some(run) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 60) else {
        return;
    };
    let replays_by = |seat: PlayerId, ordering: bool| -> u64 {
        run.replays
            .iter()
            .filter(|&&(acting, ordered, _)| acting == Some(seat) && ordered == ordering)
            .map(|&(_, _, replays)| replays)
            .sum()
    };
    assert_eq!(run.terminal, Terminal::BeatsExhausted);
    assert!(
        replays_by(P1, false) >= 1,
        "reach: P1's passes replayed the orders"
    );
    assert_eq!(replays_by(P0, false), 0);
}

/// CR 603.3b + CR 104.4b: an ordering point of Animate Dead's, Rest in Peace's and three Brain
/// Maggots' triggers, past the every-order proof's budget, stays a decision, so the loop that
/// orders Animate Dead's first is no draw.
#[test]
fn an_ordering_point_past_the_proof_budget_is_not_a_draw() {
    let board = Board {
        rest_in_peace: true,
        maggots: 3,
        first: Some("Animate Dead"),
        ..PLAIN
    };
    let Some(run) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 120) else {
        return;
    };
    assert_runs_on(&run, "Pass");
    assert_eq!(
        run.prompted("OrderTriggers"),
        1,
        "reach: the order was asked once and then saved"
    );
    assert!(
        run.replays.iter().all(|&(_, _, replays)| replays <= 1),
        "an order-only period is asked its proof without the holds replay"
    );
}

/// CR 603.6a + CR 603.3b + CR 732.4: Soul Warden returns with two Grizzly Bears, so it triggers
/// twice beside Animate Dead's trigger; every order of the three, its two triggers among
/// themselves included, continues the loop, so it is a draw.
#[test]
fn an_ordering_point_holding_one_card_twice_is_proved_in_every_order() {
    let board = Board {
        soul_warden: Some(P0),
        p0_bears: 2,
        first: Some("Animate Dead"),
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let mut buried = 0;
        let Some(run) = run_observed(board, LoopDetectionMode::Interactive, drive, 150, |state| {
            buried = buried.max(buried_triggers(state, "Soul Warden"))
        }) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            buried >= 2,
            "{drive:?}: reach: Soul Warden's triggers were buried"
        );
    }
    let mut depths = Vec::new();
    let Some(off) = run_observed(board, LoopDetectionMode::Off, Drive::Pass, 150, |state| {
        depths.push(buried_triggers(state, "Soul Warden"))
    }) else {
        return;
    };
    assert_eq!(off.terminal, Terminal::BeatsExhausted);
    assert!(
        depths[depths.len() - 1] > depths[depths.len() / 2],
        "Off: reach: the buried triggers keep growing"
    );
}

/// CR 603.3b + CR 104.4b: Soul Warden returns with four Grizzly Bears, so it triggers four times
/// beside Animate Dead's trigger; every order continues the loop, but the point's 120 orders are
/// past the every-order proof's budget, so the order stays a decision and the loop is no draw.
#[test]
fn a_continuing_loop_past_the_proof_budget_is_not_a_draw() {
    let board = Board {
        soul_warden: Some(P0),
        p0_bears: 4,
        first: Some("Animate Dead"),
        ..PLAIN
    };
    let mut widest = 0;
    let Some(run) = run_observed(
        board,
        LoopDetectionMode::Interactive,
        Drive::Pass,
        150,
        |state| {
            if let WaitingFor::OrderTriggers { triggers, .. } = &state.waiting_for {
                widest = widest.max(triggers.len());
            }
        },
    ) else {
        return;
    };
    assert_runs_on(&run, "Pass");
    assert_eq!(widest, 5, "reach: five triggers were ordered at one point");
    let mut depths = Vec::new();
    let Some(off) = run_observed(board, LoopDetectionMode::Off, Drive::Pass, 150, |state| {
        depths.push(buried_triggers(state, "Soul Warden"))
    }) else {
        return;
    };
    assert_eq!(off.terminal, Terminal::BeatsExhausted);
    assert!(
        depths[depths.len() - 1] > depths[depths.len() / 2],
        "Off: reach: the buried triggers keep growing"
    );
}

/// CR 603.3b + CR 704.5c + CR 104.4a: Ichor Rats returns beside Animate Dead each cycle and its
/// trigger, resolving first, gives each player a poison counter. The loop continues whichever
/// trigger resolves first, but poison is a loss axis, so it is not declared: both players reach
/// ten poison counters and lose together.
fn mandatory_poison_loop_is_not_declared(drive: Drive) {
    let board = Board {
        ichor_rats: true,
        first: Some("Ichor Rats"),
        ..PLAIN
    };
    for mode in MODES {
        let Some(run) = run(board, mode, drive, 250) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{mode:?} {drive:?}");
        assert_eq!(
            (run.poison(P0), run.poison(P1)),
            (10, 10),
            "{mode:?} {drive:?}: the game ended at ten poison counters"
        );
        assert!(
            run.prompted("OrderTriggers") >= 1,
            "{mode:?} {drive:?}: reach: the triggers were ordered"
        );
    }
}

#[test]
fn a_mandatory_poison_loop_whose_every_trigger_order_continues_passing_is_not_declared() {
    mandatory_poison_loop_is_not_declared(Drive::Pass);
}

#[test]
fn a_mandatory_poison_loop_whose_every_trigger_order_continues_until_turn_boundary_is_not_declared()
{
    mandatory_poison_loop_is_not_declared(Drive::UntilTurnBoundary);
}

#[test]
fn a_mandatory_poison_loop_whose_every_trigger_order_continues_until_stack_empty_is_not_declared() {
    mandatory_poison_loop_is_not_declared(Drive::UntilStackEmpty);
}

/// P0's Forest beside triggered mana sources, and Grizzly Bears in P0's hand.
#[derive(Clone, Copy, Debug)]
struct SunBoard {
    toph: bool,
    /// This many Caged Suns, each choosing `sun_color`.
    suns: usize,
    sun_color: ManaColor,
    /// The Caged Suns are tokens.
    token_suns: bool,
    /// A Gauntlet of Power choosing green.
    gauntlet: bool,
    aura: Option<ForestAura>,
}

/// An Aura enchanting the Forest; Utopia Sprawl chooses blue.
#[derive(Clone, Copy, Debug)]
enum ForestAura {
    FertileGround,
    UtopiaSprawl,
}

const TOPH_GREEN_SUN: SunBoard = SunBoard {
    toph: true,
    suns: 1,
    sun_color: ManaColor::Green,
    token_suns: false,
    gauntlet: false,
    aura: None,
};

#[derive(Clone, Copy, Debug)]
enum SunRoute {
    TapForest,
    CastAuto,
    /// A manual cast, then the Forest's tap at the payment prompt.
    CastManual,
}

struct SunRun {
    /// What the last action returned.
    returned: WaitingFor,
    /// Every action's events.
    events: Vec<GameEvent>,
    runner: GameRunner,
}

impl SunRun {
    fn produced_by(&self, name: &str) -> usize {
        let objects = &self.runner.state().objects;
        self.events
            .iter()
            .filter(|event| {
                matches!(event, GameEvent::ManaAbilityProduced { source_id, .. }
                    if objects.get(source_id).is_some_and(|object| object.name == name))
            })
            .count()
    }

    fn game_overs(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .count()
    }

    fn pool(&self, mana: ManaType) -> usize {
        self.runner.state().players[0].mana_pool.count_color(mana)
    }

    fn pool_total(&self) -> usize {
        self.runner.state().players[0].mana_pool.total()
    }
}

fn choose_color(state: &mut GameState, id: ObjectId, color: ManaColor) {
    state
        .objects
        .get_mut(&id)
        .expect("the chooser is on the battlefield")
        .chosen_attributes
        .push(ChosenAttribute::Color(color));
}

fn build_suns(
    board: SunBoard,
    mode: LoopDetectionMode,
) -> Option<(GameRunner, ObjectId, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if board.toph {
        scenario.add_real_card(P0, "Toph, the First Metalbender", Zone::Battlefield, db);
    }
    let suns: Vec<ObjectId> = (0..board.suns)
        .map(|_| scenario.add_real_card(P0, "Caged Sun", Zone::Battlefield, db))
        .collect();
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let gauntlet = board
        .gauntlet
        .then(|| scenario.add_real_card(P0, "Gauntlet of Power", Zone::Battlefield, db));
    let aura = board.aura.map(|aura| {
        let name = match aura {
            ForestAura::FertileGround => "Fertile Ground",
            ForestAura::UtopiaSprawl => "Utopia Sprawl",
        };
        (
            aura,
            scenario.add_real_card(P0, name, Zone::Battlefield, db),
        )
    });
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.loop_detection = mode;
    for &sun in &suns {
        choose_color(state, sun, board.sun_color);
        state.objects.get_mut(&sun).expect("the Sun").is_token = board.token_suns;
    }
    if let Some(gauntlet) = gauntlet {
        choose_color(state, gauntlet, ManaColor::Green);
    }
    if let Some((aura, id)) = aura {
        if matches!(aura, ForestAura::UtopiaSprawl) {
            choose_color(state, id, ManaColor::Blue);
        }
        state.objects.get_mut(&id).expect("the Aura").attached_to =
            Some(AttachTarget::Object(forest));
        state
            .objects
            .get_mut(&forest)
            .expect("the Forest")
            .attachments
            .push(id);
    }
    state.layers_dirty.mark_full();
    Some((runner, forest, bears))
}

fn forest_tap(runner: &GameRunner, forest: ObjectId) -> GameAction {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped
        .get(&forest)
        .and_then(|actions| {
            actions
                .iter()
                .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        })
        .cloned()
        .expect("the Forest can tap for mana")
}

/// Drives `route` from a fresh board.
fn drive_suns(board: SunBoard, mode: LoopDetectionMode, route: SunRoute) -> Option<SunRun> {
    let (mut runner, forest, bears) = build_suns(board, mode)?;
    let cast = |runner: &GameRunner, payment_mode| GameAction::CastSpell {
        object_id: bears,
        card_id: runner.state().objects[&bears].card_id,
        targets: vec![],
        payment_mode,
    };
    let first = match route {
        SunRoute::TapForest => forest_tap(&runner, forest),
        SunRoute::CastAuto => cast(&runner, CastPaymentMode::Auto),
        SunRoute::CastManual => cast(&runner, CastPaymentMode::Manual),
    };
    let mut result = runner.act(first).expect("the first action applies");
    let mut events = Vec::new();
    if matches!(route, SunRoute::CastManual) {
        assert!(
            matches!(result.waiting_for, WaitingFor::ManaPayment { .. }),
            "reach: the manual cast asks for payment, got {:?}",
            result.waiting_for
        );
        events.append(&mut result.events);
        let tap = forest_tap(&runner, forest);
        result = runner.act(tap).expect("the Forest's tap applies");
    }
    events.extend(result.events);
    Some(SunRun {
        returned: result.waiting_for,
        events,
        runner,
    })
}

/// CR 605.1b + CR 104.4b: under Toph, Caged Sun is a land, so the mana its own ability adds
/// triggers that ability again with no way to stop it. The game is a draw whether the Forest is
/// tapped for mana or to pay a cost.
#[test]
fn a_caged_sun_land_retriggering_itself_is_a_draw_on_every_route() {
    for route in [
        SunRoute::TapForest,
        SunRoute::CastAuto,
        SunRoute::CastManual,
    ] {
        let Some(run) = drive_suns(TOPH_GREEN_SUN, LoopDetectionMode::Interactive, route) else {
            return;
        };
        assert_eq!(
            run.produced_by("Forest"),
            1,
            "{route:?}: reach: the Forest's mana ability produced"
        );
        assert_eq!(
            run.returned,
            WaitingFor::GameOver { winner: None },
            "{route:?}"
        );
        assert_eq!(
            run.runner.state().waiting_for,
            WaitingFor::GameOver { winner: None },
            "{route:?}"
        );
        assert_eq!(
            run.runner.state().game_end,
            Some(GameEnd { winner: None }),
            "{route:?}"
        );
        assert_eq!(run.game_overs(), 1, "{route:?}");
    }
}

/// CR 104.4b: two self-retriggering Caged Suns end the game once.
#[test]
fn two_caged_suns_end_the_game_once() {
    let board = SunBoard {
        suns: 2,
        ..TOPH_GREEN_SUN
    };
    let Some(run) = drive_suns(board, LoopDetectionMode::Interactive, SunRoute::TapForest) else {
        return;
    };
    assert_eq!(
        run.produced_by("Forest"),
        1,
        "reach: the Forest's mana ability produced"
    );
    assert_eq!(run.game_overs(), 1);
    assert_eq!(run.runner.state().game_end, Some(GameEnd { winner: None }));
}

/// CR 605.1b: a Caged Sun that is not a land, or whose color the Forest does not add, does not
/// retrigger itself, and an Aura's triggered mana ability is not a land's ability; each adds one
/// mana. Only where loop detection samples is a triggered mana ability's production an event.
#[test]
fn one_extra_mana_without_a_self_retrigger() {
    let no_toph = SunBoard {
        toph: false,
        ..TOPH_GREEN_SUN
    };
    let red_sun = SunBoard {
        sun_color: ManaColor::Red,
        ..TOPH_GREEN_SUN
    };
    let fertile = SunBoard {
        aura: Some(ForestAura::FertileGround),
        ..no_toph
    };
    let utopia = SunBoard {
        aura: Some(ForestAura::UtopiaSprawl),
        ..no_toph
    };
    // (board, the triggered source whose production is an event, green, blue, total)
    let legs = [
        (no_toph, Some("Caged Sun"), 2, 0, 2),
        (red_sun, None, 1, 0, 1),
        (fertile, Some("Fertile Ground"), 2, 0, 3),
        (utopia, Some("Utopia Sprawl"), 2, 1, 3),
    ];
    for mode in MODES {
        for (board, producer, green, blue, total) in legs {
            let leg = format!("{mode:?} {board:?}");
            let Some(run) = drive_suns(board, mode, SunRoute::TapForest) else {
                return;
            };
            assert_eq!(
                run.produced_by("Forest"),
                1,
                "{leg}: reach: the Forest's mana ability produced"
            );
            assert!(
                matches!(run.returned, WaitingFor::Priority { .. }),
                "{leg}: got {:?}",
                run.returned
            );
            assert_eq!(
                (
                    run.pool(ManaType::Green),
                    run.pool(ManaType::Blue),
                    run.pool_total()
                ),
                (green, blue, total),
                "{leg}"
            );
            match (mode.samples(), producer) {
                (true, Some(producer)) => {
                    assert_eq!(run.produced_by(producer), 1, "{leg}: {producer}")
                }
                (true, None) => assert_eq!(run.produced_by("Caged Sun"), 0, "{leg}"),
                (false, _) => assert_eq!(
                    ["Caged Sun", "Fertile Ground", "Utopia Sprawl"]
                        .map(|name| run.produced_by(name)),
                    [0; 3],
                    "{leg}"
                ),
            }
        }
    }
}

/// CR 605.1b: under Toph, Gauntlet of Power is a land and a token Caged Sun is not, so the Sun
/// adds one mana for the Forest's ability and one for Gauntlet's, and its own triggers nothing.
/// Off forgoes the production event, so the Sun misses Gauntlet's ability.
#[test]
fn a_token_caged_sun_under_toph_adds_once_per_land_ability() {
    let board = SunBoard {
        token_suns: true,
        gauntlet: true,
        ..TOPH_GREEN_SUN
    };
    for (mode, green, gauntlet, sun) in [
        (LoopDetectionMode::Interactive, 4, 1, 2),
        (LoopDetectionMode::Off, 3, 0, 0),
    ] {
        let Some(run) = drive_suns(board, mode, SunRoute::TapForest) else {
            return;
        };
        assert_eq!(
            run.produced_by("Forest"),
            1,
            "{mode:?}: reach: the Forest's mana ability produced"
        );
        assert!(
            matches!(run.returned, WaitingFor::Priority { .. }),
            "{mode:?}: got {:?}",
            run.returned
        );
        assert_eq!(run.pool(ManaType::Green), green, "{mode:?}");
        assert_eq!(
            (
                run.produced_by("Gauntlet of Power"),
                run.produced_by("Caged Sun")
            ),
            (gauntlet, sun),
            "{mode:?}"
        );
        assert_eq!(run.game_overs(), 0, "{mode:?}");
    }
}

fn dragon_trigger_on_top(state: &GameState) -> bool {
    state.stack.back().is_some_and(|top| {
        matches!(&top.kind, StackEntryKind::TriggeredAbility { source_id, .. }
            if state.objects.get(source_id).is_some_and(|o| o.name == "Worldgorger Dragon"))
    })
}

fn bolts_in_hand(state: &GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Lightning Bolt")
        })
        .count()
}

/// The Lightning Bolt board, passed until the engine offers P0 a draw without end.
fn at_a_without_end_offer() -> Option<GameRunner> {
    let board = Board {
        optional: true,
        ..PLAIN
    };
    let Built { mut runner, .. } = build(board, LoopDetectionMode::Interactive)?;
    for _ in 0..60 {
        let action = match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => return Some(runner),
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("a legal drive action");
    }
    panic!("the engine never offered the loop");
}

/// Passes priority and orders triggers until another prompt opens, returning it.
fn pass_on(runner: &mut GameRunner, beats: usize) -> WaitingFor {
    for _ in 0..beats {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            _ => break,
        };
        runner.act(action).expect("a legal drive action");
    }
    runner.state().waiting_for.clone()
}

/// P1 casts its Lightning Bolt at P0 and it resolves; the loop then has no out left and the
/// players pass on to a draw (CR 104.4b + CR 732.4).
fn bolt_p0_then_draw(runner: &mut GameRunner) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P1),
        "{:?}",
        state.waiting_for
    );
    let bolt = state.players[P1.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|id| state.objects[id].name == "Lightning Bolt")
        .expect("P1 holds its Bolt");
    drop(runner.cast(bolt).target_player(P0).commit());
    assert_eq!(pass_on(runner, 200), WaitingFor::GameOver { winner: None });
    let state = runner.state();
    assert_eq!(state.players[P0.0 as usize].life, 17, "the Bolt resolved");
    assert_eq!(state.objects[&bolt].zone, Zone::Graveyard);
}

fn dragon_entries(runner: &GameRunner, events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
                if runner.state().objects.get(object_id)
                    .is_some_and(|o| o.name == "Worldgorger Dragon"))
        })
        .count()
}

const DECLARE_WITHOUT_END: GameAction = GameAction::DeclareShortcut {
    count: IterationCount::WithoutEnd,
    template: None,
};

/// CR 732.2a: the offer to drain P0 published no "without end", so declaring one is refused.
#[test]
fn a_proposal_without_end_is_refused_on_an_offer_that_did_not_publish_one() {
    let with_out = Board {
        archer: true,
        p0_out: true,
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let Some(mut run) = run(with_out, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        match &run.state().waiting_for {
            WaitingFor::LoopShortcut {
                proposer,
                predicted_winner,
                schema,
                ..
            } => {
                assert_eq!(*proposer, P0, "{drive:?}");
                assert_eq!(*predicted_winner, Some(P1), "{drive:?}");
                assert_eq!(schema.iteration_count, IterationCount::UntilLethal);
            }
            other => panic!("{drive:?}: no offer: {other:?}"),
        }
        let stack = run.state().stack.len();
        let mut control = run.state().clone();
        let until_lethal = apply(
            &mut control,
            P0,
            GameAction::DeclareShortcut {
                count: IterationCount::UntilLethal,
                template: None,
            },
        )
        .expect("the offer's own count is declarable");
        assert!(
            matches!(
                until_lethal.waiting_for,
                WaitingFor::RespondToShortcut { .. }
            ),
            "{drive:?}: {:?}",
            until_lethal.waiting_for
        );

        run.runner
            .act(DECLARE_WITHOUT_END)
            .expect("a refused declaration hands priority back");
        let state = run.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
            "{drive:?}: {:?}",
            state.waiting_for
        );
        assert_eq!(state.stack.len(), stack, "{drive:?}");
    }
}

/// CR 104.4b + CR 732.4: once every player accepts a proposal to repeat this mandatory loop
/// without end, the game is a draw and nothing the loop does is performed.
#[test]
fn a_proposal_without_end_every_player_accepts_is_a_draw() {
    let Some(mut runner) = at_a_without_end_offer() else {
        return;
    };
    assert_eq!(
        legal_actions(runner.state()),
        vec![DECLARE_WITHOUT_END, GameAction::DeclineShortcut]
    );
    assert_eq!(bolts_in_hand(runner.state(), P1), 1);

    runner.act(DECLARE_WITHOUT_END).expect("declare");
    match &runner.state().waiting_for {
        WaitingFor::RespondToShortcut {
            player,
            remaining_players,
            proposal,
        } => {
            assert_eq!(*player, P1);
            assert!(remaining_players.is_empty());
            assert_eq!(proposal.count, IterationCount::WithoutEnd);
        }
        other => panic!("no response window: {other:?}"),
    }

    let accepted = runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: None }
    );
    assert_eq!(
        accepted
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { winner: None }))
            .count(),
        1
    );
    assert_eq!(bolts_in_hand(runner.state(), P1), 1);
}

/// CR 732.2b + CR 732.2c: shortened at the current place, the proposal takes nothing and the
/// shortener holds priority over the same stack.
#[test]
fn a_proposal_without_end_shortened_at_the_current_place_gives_the_shortener_priority() {
    let Some(mut runner) = at_a_without_end_offer() else {
        return;
    };
    let stack = runner.state().stack.len();
    runner.act(DECLARE_WITHOUT_END).expect("declare");
    let shortened = runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 0 },
        })
        .expect("place 0 is a place a responder may name");
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P1),
        "{:?}",
        state.waiting_for
    );
    assert!(dragon_trigger_on_top(state));
    assert_eq!(state.stack.len(), stack);
    assert!(!shortened.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            to: Zone::Battlefield,
            ..
        }
    )));
    bolt_p0_then_draw(&mut runner);
}

/// CR 732.2b + CR 732.2c: shortened at a later place, the proposal performs the loop up to it
/// and the shortener holds priority there.
#[test]
fn a_proposal_without_end_shortened_at_a_later_place_drives_the_loop_to_it() {
    let Some(mut runner) = at_a_without_end_offer() else {
        return;
    };
    runner.act(DECLARE_WITHOUT_END).expect("declare");
    let shortened = runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 2 },
        })
        .expect("place 2 is a place a responder may name");
    assert_eq!(dragon_entries(&runner, &shortened.events), 2);
    assert!(dragon_trigger_on_top(runner.state()));
    bolt_p0_then_draw(&mut runner);
}

/// The counted take the offer or the proposal at `state` carries.
pub(crate) fn counted_take(state: &GameState) -> CountedTake {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { counted_take, .. } => *counted_take,
        WaitingFor::RespondToShortcut { proposal, .. } => proposal.counted_take,
        other => panic!("no offer or proposal: {other:?}"),
    }
}

type WindowBoard = (
    WaitingFor,
    Vec<ObjectId>,
    BTreeMap<ObjectId, (Zone, std::collections::HashMap<CounterType, u32>)>,
);

/// The prompt, the stack, and every object's zone and counters.
fn window_board(state: &GameState) -> WindowBoard {
    (
        state.waiting_for.clone(),
        state.stack.iter().map(|entry| entry.id).collect(),
        state
            .objects
            .iter()
            .map(|(id, object)| (*id, (object.zone, object.counters.clone())))
            .collect(),
    )
}

/// Asserts `seat`'s `action` at `state` is refused as invalid for `refusal` and leaves the prompt,
/// the stack, and every zone and counter as they were.
fn assert_refused(state: &GameState, seat: PlayerId, action: GameAction, refusal: &str, leg: &str) {
    let mut probe = state.clone();
    let refused = apply(&mut probe, seat, action.clone()).err();
    assert!(
        matches!(&refused, Some(EngineError::InvalidAction(message)) if message.contains(refusal)),
        "{leg}: {action:?} -> {refused:?}"
    );
    assert!(
        window_board(&probe) == window_board(state),
        "{leg}: the refused {action:?} moved the board"
    );
}

/// `seat`'s `action` applied to a copy of `state`: the board after it, and its events.
fn admitted(
    state: &GameState,
    seat: PlayerId,
    action: GameAction,
    leg: &str,
) -> (GameState, Vec<GameEvent>) {
    let mut next = state.clone();
    let applied = apply(&mut next, seat, action.clone())
        .unwrap_or_else(|error| panic!("{leg}: {action:?} -> {error:?}"));
    (next, applied.events)
}

fn declare(count: IterationCount) -> GameAction {
    GameAction::DeclareShortcut {
        count,
        template: None,
    }
}

fn shorten_at(at_iteration: u32) -> GameAction {
    GameAction::RespondToShortcut {
        response: ShortcutResponse::Shorten { at_iteration },
    }
}

fn priority_to(state: &GameState, player: PlayerId) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player: holder } if holder == player)
}

fn responds(state: &GameState, player: PlayerId) -> bool {
    matches!(&state.waiting_for, WaitingFor::RespondToShortcut { player: responder, .. } if *responder == player)
}

fn battlefield_entries(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    to: Zone::Battlefield,
                    ..
                }
            )
        })
        .count()
}

/// The repetitions a take performed: the record elides the middle ones but keeps the last marker.
fn repetitions(events: &[GameEvent]) -> u32 {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ShortcutRepetition { repetition, .. } => Some(*repetition),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

const NO_WINNER: &str = "naming no winner";

/// Asserts each player's view of the offer at `offered` carries the engine's counted take.
fn assert_viewers_read_the_take(offered: &GameState, leg: &str) {
    for viewer in [P0, P1] {
        assert_eq!(
            counted_take(&filter_state_for_viewer(offered, viewer)),
            counted_take(offered),
            "{leg}: {viewer:?}"
        );
    }
}

/// Asserts the offer at `offered` carries `take`, so P0's counted and until-lethal declarations
/// and P1's shortens past the current place are refused with the board as it was, while a
/// declaration without end is admitted and a shorten at the current place gives P1 priority.
pub(crate) fn assert_no_counted_take(offered: &GameState, take: CountedTake, leg: &str) {
    let CountedTake::Unperformable(cause) = take else {
        panic!("{leg}: a take that performs takes counts");
    };
    assert_eq!(counted_take(offered), take, "{leg}");
    assert_viewers_read_the_take(offered, leg);
    assert_refused(
        offered,
        P0,
        declare(IterationCount::Fixed(3)),
        cause.reason(),
        leg,
    );
    assert_refused(
        offered,
        P0,
        declare(IterationCount::UntilLethal),
        NO_WINNER,
        leg,
    );
    let (window, _) = admitted(offered, P0, DECLARE_WITHOUT_END, leg);
    assert!(responds(&window, P1), "{leg}: {:?}", window.waiting_for);
    assert_eq!(
        counted_take(&window),
        take,
        "{leg}: the proposal copies the take"
    );
    for place in [1, 2] {
        assert_refused(&window, P1, shorten_at(place), cause.reason(), leg);
    }
    let (current, _) = admitted(&window, P1, shorten_at(0), leg);
    assert!(
        priority_to(&current, P1),
        "{leg}: {:?}",
        current.waiting_for
    );
}

/// Asserts the offer at `offered` carries a counted take that performs: a proposal without end or
/// a declared `Fixed(3)` shortened at place 2 drives two cycles and gives P1 priority, the
/// accepted `Fixed(3)` drives three, and `UntilLethal`, naming no winner, is refused.
fn assert_counted_take_performs(offered: &GameState, leg: &str) {
    assert_eq!(counted_take(offered), CountedTake::Performs, "{leg}");
    assert_viewers_read_the_take(offered, leg);
    assert_refused(
        offered,
        P0,
        declare(IterationCount::UntilLethal),
        NO_WINNER,
        leg,
    );
    let (without_end, _) = admitted(offered, P0, DECLARE_WITHOUT_END, leg);
    let (_, one) = admitted(&without_end, P1, shorten_at(1), leg);
    let cycle = battlefield_entries(&one);
    assert!(cycle > 0, "{leg}: reach: a cycle enters permanents");
    let (fixed, _) = admitted(offered, P0, declare(IterationCount::Fixed(3)), leg);
    for window in [&without_end, &fixed] {
        assert!(responds(window, P1), "{leg}: {:?}", window.waiting_for);
        let (at_two, events) = admitted(window, P1, shorten_at(2), leg);
        assert_eq!(battlefield_entries(&events), 2 * cycle, "{leg}");
        assert_eq!(repetitions(&events), 2, "{leg}");
        assert!(priority_to(&at_two, P1), "{leg}: {:?}", at_two.waiting_for);
    }
    let (_, accepted) = admitted(
        &fixed,
        P1,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        },
        leg,
    );
    assert_eq!(repetitions(&accepted), 3, "{leg}");
}

/// CR 732.2b + CR 732.2c: on the Lightning Bolt board, beside P0's Soul Warden with its trigger
/// ordered first or not, and over Wood Elves' shuffle, a counted take performs on every drive,
/// and `UntilLethal` names no winner.
#[test]
fn a_without_end_offer_the_drain_delimits_takes_every_count() {
    let warden = Board {
        optional: true,
        soul_warden: Some(P0),
        ..PLAIN
    };
    for board in [
        Board {
            optional: true,
            ..PLAIN
        },
        warden,
        Board {
            first: Some("Soul Warden"),
            ..warden
        },
        WOOD_ELVES,
    ] {
        for drive in EVERY_DRIVE {
            let leg = format!(
                "warden={:?} first={:?} wood_elves={:?} {drive:?}",
                board.soul_warden, board.first, board.wood_elves
            );
            let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
                return;
            };
            assert_out_offer(&offered, &leg);
            assert_counted_take_performs(offered.state(), &leg);
        }
    }
}

/// CR 732.2a: a declined offer hands P0 priority; P1 may then use its out, after which the loop
/// is a draw, and a loop left alone is offered again.
#[test]
fn a_declined_offer_without_end_leaves_the_out_and_is_offered_again() {
    let Some(mut runner) = at_a_without_end_offer() else {
        return;
    };
    runner.act(GameAction::DeclineShortcut).expect("decline");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    bolt_p0_then_draw(&mut runner);

    let Some(mut again) = at_a_without_end_offer() else {
        return;
    };
    again.act(GameAction::DeclineShortcut).expect("decline");
    assert!(
        matches!(pass_on(&mut again, 60), WaitingFor::LoopShortcut { .. }),
        "{:?}",
        again.state().waiting_for
    );
}

/// CR 732.2a + CR 732.2b: the interaction publishes the proposal's count, answers its
/// acceptance with the declaration, and offers the responder every place to shorten it.
#[test]
fn a_proposal_without_end_is_published_and_answered_through_the_interaction() {
    let Some(mut runner) = at_a_without_end_offer() else {
        return;
    };
    let mut offered = runner.state().clone();
    bind_interaction_authority(
        &mut offered,
        InteractionSessionId("without-end-offer".into()),
    )
    .expect("bind");
    let view = derive_viewer_interaction(&offered, &filter_state_for_viewer(&offered, P0), P0);
    assert_eq!(view.availability, InteractionAvailability::InputRequired);
    let opportunity = view.opportunities.first().expect("P0 is asked");
    assert!(
        matches!(
            &opportunity.response,
            InteractionOpportunityResponse::Schema {
                spec: InteractionResponseSpec::Shortcut {
                    count: InteractionShortcutCountSpec::WithoutEnd,
                    points,
                    allow_decline: true,
                    ..
                },
                ..
            } if points.is_empty()
        ),
        "{:?}",
        opportunity.response
    );
    let resolved = resolve_interaction_response(
        &offered,
        P0,
        &InteractionSubmission {
            interaction_id: opportunity.interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::AcceptSuggested,
                pins: Vec::new(),
            },
        },
    )
    .expect("accepting the suggestion resolves");
    assert_eq!(resolved, DECLARE_WITHOUT_END);

    runner.act(DECLARE_WITHOUT_END).expect("declare");
    let mut declared = runner.state().clone();
    bind_interaction_authority(
        &mut declared,
        InteractionSessionId("without-end-reply".into()),
    )
    .expect("bind");
    let reply = derive_viewer_interaction(&declared, &filter_state_for_viewer(&declared, P1), P1);
    let opportunity = reply.opportunities.first().expect("P1 is asked");
    assert!(
        matches!(
            opportunity.response,
            InteractionOpportunityResponse::Schema {
                spec: InteractionResponseSpec::ShortcutReply {
                    min_iteration: 0,
                    max_iteration: u32::MAX,
                    ..
                },
                ..
            }
        ),
        "{:?}",
        opportunity.response
    );
}

/// CR 603.3b + CR 701.24a: with Animate Dead's trigger first, Wood Elves' trigger is buried each
/// cycle, and the order that resolves it first shuffles a library no player knows, which recurs by
/// its cards, so every order continues and the loop is a draw; Off runs on.
#[test]
fn a_shuffle_buried_under_animate_deads_trigger_is_a_draw() {
    let board = Board {
        wood_elves: Some(10),
        first: Some("Animate Dead"),
        ..PLAIN
    };
    for drive in EVERY_DRIVE {
        let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 40) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::GameOver(None), "{drive:?}");
        assert!(
            run.prompted("OrderTriggers") >= 1,
            "{drive:?}: reach: the triggers were ordered"
        );
        assert!(run.entries >= 1, "{drive:?}: reach: the Dragon entered");
    }
    for drive in EVERY_DRIVE {
        if run_off(board, drive, 40).is_none() {
            return;
        }
    }
}

const WOOD_ELVES: Board = Board {
    wood_elves: Some(10),
    ..PLAIN
};

/// Asserts every drive at `board` stops at P0's offer of a draw without end over its shuffled
/// library, that every player accepting it draws the game, and that Off runs on.
fn a_shuffle_is_offered(board: Board) {
    let Some(offered) = offered_on_every_drive(
        board,
        &[Pick::First],
        "OrderTriggers",
        1,
        CountedTake::Performs,
    ) else {
        return;
    };
    let state = offered.state();
    assert_eq!(
        state.library_of(P0).len(),
        10,
        "reach: P0's library kept its cards"
    );
    assert!(
        state.rng.get_word_pos() > 0,
        "reach: the shuffles drew randomness"
    );
    offer_accepted_is_a_draw(offered);
    for drive in EVERY_DRIVE {
        if run_off(board, drive, 40).is_none() {
            return;
        }
    }
}

/// CR 701.24a + CR 732.2a: Wood Elves' trigger finds no Forest and shuffles P0's library each
/// cycle, and nothing reads the order, so the period differs only in randomness and is offered,
/// never declared.
#[test]
fn a_shuffle_no_player_observes_is_offered() {
    a_shuffle_is_offered(WOOD_ELVES);
}

/// CR 701.24a + CR 732.2a: P1's Lightning Bolt beside the shuffle is an out, so the period is
/// offered.
#[test]
fn a_shuffle_beside_an_out_is_offered() {
    a_shuffle_is_offered(Board {
        optional: true,
        ..WOOD_ELVES
    });
}

/// CR 104.4b + CR 732.4: shuffling a one-card library draws no randomness, so the period repeats
/// exactly and is a draw; Off runs on.
#[test]
fn a_shuffle_of_a_one_card_library_is_a_draw() {
    let board = Board {
        wood_elves: Some(1),
        ..PLAIN
    };
    let Some(run) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 40) else {
        return;
    };
    assert_eq!(run.terminal, Terminal::GameOver(None));
    assert!(run.entries >= 2, "reach: the Dragon entered at least twice");
    assert_eq!(run.state().rng.get_word_pos(), 0, "reach: no shuffle drew");
    run_off(board, Drive::Pass, 40);
}

/// CR 732.2b + CR 701.24a: beside Wood Elves' shuffle, Poison-Tip Archer's drain is offered with
/// P1 as its predicted winner, not won outright.
#[test]
fn a_drain_beside_a_shuffle_is_offered_not_won() {
    let board = Board {
        archer: true,
        ..WOOD_ELVES
    };
    for drive in EVERY_DRIVE {
        let Some(run) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert!(
            matches!(
                run.terminal,
                Terminal::Offer {
                    predicted_winner: Some(P1),
                    ..
                }
            ),
            "{drive:?}: {:?}",
            run.terminal
        );
        assert!(
            run.state().rng.get_word_pos() > 0,
            "{drive:?}: reach: the shuffles drew randomness"
        );
        assert!(
            (1..20).contains(&run.life(P0)),
            "{drive:?}: reach: P0 was drained before the drain ended"
        );
    }
}

const RAPTOR_DRIVES: [Drive; 4] = [
    Drive::Pass,
    Drive::UntilTurnBoundary,
    Drive::UntilStackEmpty,
    Drive::P0UntilStackEmpty,
];

/// `seat` casts its Lightning Bolt at Marauding Raptor; the Raptor dies and the turn moves on to
/// combat.
fn bolt_the_raptor_and_play_on(runner: &mut GameRunner, seat: PlayerId) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == seat),
        "{:?}",
        state.waiting_for
    );
    let bolt = state.players[seat.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|id| state.objects[id].name == "Lightning Bolt")
        .expect("the seat holds its Bolt");
    let raptor = state
        .battlefield
        .iter()
        .copied()
        .find(|id| state.objects[id].name == "Marauding Raptor")
        .expect("the Raptor is on the battlefield");
    drop(runner.cast(bolt).target_object(raptor).commit());
    assert!(
        matches!(pass_on(runner, 200), WaitingFor::DeclareAttackers { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    let state = runner.state();
    assert_eq!(state.objects[&raptor].zone, Zone::Graveyard);
    assert_eq!(state.objects[&bolt].zone, Zone::Graveyard);
}

fn raptor_copies_keep_coming_off(board: Board) {
    for drive in RAPTOR_DRIVES {
        let Some(run) = run(board, LoopDetectionMode::Off, drive, 60) else {
            return;
        };
        assert_eq!(run.terminal, Terminal::BeatsExhausted, "Off {drive:?}");
        assert!(
            run.on_battlefield("Polyraptor").len() >= 4,
            "Off {drive:?}: reach: the copies kept coming"
        );
    }
}

fn raptor_full_yields_are_offered(board: Board) {
    let Some(terminal) = full_yield_drive_returns(board, 6, |run| run.terminal) else {
        return;
    };
    assert_eq!(
        terminal,
        Terminal::Offer {
            predicted_winner: None,
            mandatory: false
        },
        "FullYields"
    );
}

/// CR 732.5 + CR 732.2a: P1's Lightning Bolt kills Marauding Raptor (CR 704.5g), so the
/// token-copy loop is offered to P0 on every drive and accepting it draws; Off runs on.
#[test]
fn a_token_copy_loop_beside_p1s_bolt_is_offered() {
    let board = Board {
        optional: true,
        ..RAPTOR
    };
    for drive in RAPTOR_DRIVES {
        let leg = format!("{drive:?}");
        let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert_out_offer(&offered, &leg);
        assert!(
            offered.on_battlefield("Polyraptor").len() >= 2,
            "{leg}: reach: a copy entered"
        );
        offer_accepted_is_a_draw(offered);

        let Some(mut shortened) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        let stack = shortened.state().stack.len();
        shortened.runner.act(DECLARE_WITHOUT_END).expect("declare");
        shortened
            .runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Shorten { at_iteration: 0 },
            })
            .expect("the current place");
        assert_eq!(shortened.state().stack.len(), stack, "{leg}");
        bolt_the_raptor_and_play_on(&mut shortened.runner, P1);
    }
    raptor_full_yields_are_offered(board);
    raptor_copies_keep_coming_off(board);
}

/// CR 732.5 + CR 732.2a: P0's Lightning Bolt kills Marauding Raptor (CR 704.5g), so the
/// token-copy loop is offered to P0 on every drive, accepting it draws, and declining it lets P0
/// end it by play; Off runs on.
#[test]
fn a_token_copy_loop_beside_p0s_bolt_is_offered() {
    let board = Board {
        p0_out: true,
        ..RAPTOR
    };
    for drive in RAPTOR_DRIVES {
        let leg = format!("{drive:?}");
        let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        assert_out_offer(&offered, &leg);
        assert!(
            offered.on_battlefield("Polyraptor").len() >= 2,
            "{leg}: reach: a copy entered"
        );
        offer_accepted_is_a_draw(offered);

        let Some(mut declined) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        declined
            .runner
            .act(GameAction::DeclineShortcut)
            .expect("decline");
        bolt_the_raptor_and_play_on(&mut declined.runner, P0);
    }
    raptor_full_yields_are_offered(board);
    raptor_copies_keep_coming_off(board);
}

/// A period the recording shows without progress is refused before any replay, and a growth
/// period is replayed.
#[test]
fn a_period_recorded_without_progress_is_offered_without_a_replay() {
    for (board, replayed) in [
        (
            Board {
                optional: true,
                ..PLAIN
            },
            false,
        ),
        (
            Board {
                optional: true,
                ..RAPTOR
            },
            true,
        ),
    ] {
        for drive in EVERY_DRIVE {
            let leg = format!("raptor={} {drive:?}", board.raptor);
            let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
                return;
            };
            assert_out_offer(&offered, &leg);
            let replays: u64 = offered.replays.iter().map(|&(_, _, n)| n).sum();
            assert_eq!(replays > 0, replayed, "{leg}: {replays} replays");
        }
    }
}

/// CR 701.24a + CR 732.2a: with no out held, the shuffle's recorded period shows neither
/// progress nor stack growth, so it is offered without the witness replaying it; Board B with no
/// out is replayed and declared.
#[test]
fn an_offered_period_recorded_without_progress_is_not_replayed() {
    for drive in EVERY_DRIVE {
        let leg = format!("{drive:?}");
        let before = play_trace_counters();
        let Some(offered) = run(WOOD_ELVES, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        let replays = play_trace_counters().since(before).mandatory_replays;
        assert_out_offer(&offered, &leg);
        assert_eq!(replays, 0, "{leg}");

        let before = play_trace_counters();
        let Some(declared) = run(RAPTOR, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        let replays = play_trace_counters().since(before).mandatory_replays;
        assert_eq!(declared.terminal, Terminal::GameOver(None), "{leg}");
        assert!(replays >= 1, "{leg}: reach: the growth period was replayed");
    }
}

/// CR 701.24a + CR 732.2a: Wood Elves' shuffle each cycle beside Lumberknot's +1/+1 counter at
/// each Dragon death, with no out held, is offered without end on every drive and accepting it
/// draws; Off runs on.
fn shuffle_beside_counter_growth_is_offered(drive: Drive) {
    let board = Board {
        lumberknot: true,
        ..WOOD_ELVES
    };
    let leg = format!("{drive:?}");
    let before = play_trace_counters();
    let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
        return;
    };
    let replays = play_trace_counters().since(before).mandatory_replays;
    assert_out_offer(&offered, &leg);
    assert!(
        replays >= 1,
        "{leg}: reach: the witness replayed the period"
    );
    assert!(
        offered.counters_on("Lumberknot", CounterType::Plus1Plus1) >= 1,
        "{leg}: reach: Lumberknot grew"
    );
    assert!(
        offered.state().rng.get_word_pos() > 0,
        "{leg}: reach: the shuffles drew randomness"
    );
    offer_accepted_is_a_draw(offered);
    run_off(board, drive, 60);
}

#[test]
fn a_shuffle_beside_counter_growth_nothing_reads_passing_is_offered() {
    shuffle_beside_counter_growth_is_offered(Drive::Pass);
}

#[test]
fn a_shuffle_beside_counter_growth_nothing_reads_until_turn_boundary_is_offered() {
    shuffle_beside_counter_growth_is_offered(Drive::UntilTurnBoundary);
}

#[test]
fn a_shuffle_beside_counter_growth_nothing_reads_until_stack_empty_is_offered() {
    shuffle_beside_counter_growth_is_offered(Drive::UntilStackEmpty);
}

/// Board B with an out on either seat recurs only on its token growth, which the ring drain does
/// not delimit, so on every drive no count above zero is taken.
#[test]
fn a_token_copy_offer_beside_an_out_refuses_every_counted_take() {
    for board in [
        Board {
            optional: true,
            ..RAPTOR
        },
        Board {
            p0_out: true,
            ..RAPTOR
        },
    ] {
        for drive in RAPTOR_DRIVES {
            let leg = format!("p0_out={} {drive:?}", board.p0_out);
            let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
                return;
            };
            assert_out_offer(&offered, &leg);
            assert!(
                offered.on_battlefield("Polyraptor").len() >= 2,
                "{leg}: reach: a copy entered"
            );
            assert_no_counted_take(
                offered.state(),
                CountedTake::Unperformable(UnperformableCause::UndelimitedCover),
                &leg,
            );
        }
    }
}

/// CR 732.2b: a counted proposal over Banisher Priest's choice reaches a response window only by a
/// restore, since its declaration is refused; restored, it keeps the offer's counted take, a later
/// place is refused with the window intact, and the current place gives P1 priority.
#[test]
fn a_restored_counted_proposal_over_a_choice_refuses_a_later_place() {
    let choice = UnperformableCause::UncontainedChoice;
    for board in [
        TWO_TARGETS,
        Board {
            first: Some("Animate Dead"),
            ..TWO_TARGETS
        },
    ] {
        let Some(offered) = run(board, LoopDetectionMode::Interactive, Drive::Pass, 60) else {
            return;
        };
        let leg = format!("{:?}", board.first);
        assert_out_offer(&offered, &leg);
        let (window, _) = admitted(offered.state(), P0, DECLARE_WITHOUT_END, &leg);
        for count in [IterationCount::Fixed(3), IterationCount::UntilLethal] {
            let leg = format!("{leg} {count:?}");
            let mut counted = window.clone();
            let WaitingFor::RespondToShortcut { proposal, .. } = &mut counted.waiting_for else {
                panic!("{leg}: {:?}", counted.waiting_for);
            };
            proposal.count = count.clone();
            let restored = serde_json::from_value::<PersistedGameState>(
                serde_json::to_value(&counted).expect("the window encodes"),
            )
            .expect("the window decodes")
            .into_game_state()
            .expect("the window restores");
            let WaitingFor::RespondToShortcut { proposal, .. } = &restored.waiting_for else {
                panic!("{leg}: {:?}", restored.waiting_for);
            };
            assert_eq!(proposal.count, count, "{leg}: reach: the counted proposal");
            assert_eq!(
                counted_take(&restored),
                CountedTake::Unperformable(choice),
                "{leg}"
            );
            assert_refused(&restored, P1, shorten_at(2), choice.reason(), &leg);
            let (current, _) = admitted(&restored, P1, shorten_at(0), &leg);
            assert!(
                priority_to(&current, P1),
                "{leg}: {:?}",
                current.waiting_for
            );
        }
    }
}

/// CR 701.24a + CR 732.2a: beside Soul Warden on either seat, the period the play trace names
/// holds no shuffle while its replay shuffles P0's library, so the period is offered, never
/// declared, and every player accepting it draws the game; Off runs on. The ring drain's
/// delimiter fires in the replay beside P1's Soul Warden, so a count is taken, and fires nowhere
/// beside P0's, so no count above zero is taken.
fn shuffle_only_the_replay_draws_is_offered(drive: Drive) {
    for (seat, refused) in [(P1, None), (P0, Some(UnperformableCause::UndelimitedCover))] {
        let board = Board {
            soul_warden: Some(seat),
            ..WOOD_ELVES
        };
        let leg = format!("{seat:?} {drive:?}");
        let before = play_trace_counters();
        let Some(offered) = run(board, LoopDetectionMode::Interactive, drive, 60) else {
            return;
        };
        let replays = play_trace_counters().since(before).mandatory_replays;
        assert_out_offer(&offered, &leg);
        assert!(
            replays >= 1,
            "{leg}: reach: the witness replayed the period"
        );
        assert!(
            offered.state().rng.get_word_pos() > 0,
            "{leg}: reach: the shuffles drew randomness"
        );
        match refused {
            None => assert_counted_take_performs(offered.state(), &leg),
            Some(cause) => {
                assert_no_counted_take(offered.state(), CountedTake::Unperformable(cause), &leg)
            }
        }
        offer_accepted_is_a_draw(offered);
        run_off(board, drive, 60);
    }
}

#[test]
fn a_shuffle_only_the_replay_draws_passing_is_offered() {
    shuffle_only_the_replay_draws_is_offered(Drive::Pass);
}

#[test]
fn a_shuffle_only_the_replay_draws_until_turn_boundary_is_offered() {
    shuffle_only_the_replay_draws_is_offered(Drive::UntilTurnBoundary);
}

#[test]
fn a_shuffle_only_the_replay_draws_until_stack_empty_is_offered() {
    shuffle_only_the_replay_draws_is_offered(Drive::UntilStackEmpty);
}

/// Banisher Priest against two Grizzly Bears, passed to P0's offer and declared without end: P1's
/// response window over a proposal no count above zero is taken from.
pub(crate) fn two_targets_without_end_window() -> Option<GameState> {
    let offered = run(TWO_TARGETS, LoopDetectionMode::Interactive, Drive::Pass, 60)?;
    assert_out_offer(&offered, "two targets");
    Some(admitted(offered.state(), P0, DECLARE_WITHOUT_END, "two targets").0)
}
