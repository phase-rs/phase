//! CR 603.3b + CR 607.2a: a creature that dies to a state-based action right
//! after entering puts its enters and leaves triggers on the stack together;
//! when the enters trigger is ordered on top, the leaves trigger refers to the
//! card the enters trigger exiled.
//!
//! Oracle text (Scryfall):
//!   Wormfang Newt: "When this creature enters, exile a land you control.
//!    When this creature leaves the battlefield, return the exiled card to the
//!    battlefield under its owner's control."
//!   Skyclave Apparition: "When this creature enters, exile up to one target
//!    nonland, nontoken permanent you don't control with mana value 4 or less.
//!    When this creature leaves the battlefield, the exiled card's owner
//!    creates an X/X blue Illusion creature token, where X is the mana value of
//!    the exiled card."
//!   Collected Company: "Look at the top six cards of your library. Put up to
//!    two creature cards with mana value 3 or less from among them onto the
//!    battlefield. Put the rest on the bottom of your library in any order."
//!   Elesh Norn, Grand Cenobite: "Vigilance / Other creatures you control get
//!    +2/+2. / Creatures your opponents control get -2/-2."
//! Ruling (Fiend Hunter, 2018-12-07): if Fiend Hunter leaves the battlefield
//! before its first ability resolves, its second ability "will trigger and do
//! nothing. Then its first ability will resolve and exile the target creature
//! indefinitely."
//!
//! CR 117.5 + CR 603.3b: every ability a post-action pass collects — on the
//! action, on its state-based actions, on exile returns, delayed and state
//! triggers — is placed as one batch after the state-based actions, before the
//! next priority. The rows below drive one
//! member of that class each through real card data.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, LoopDetectionMode, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const MODES: [LoopDetectionMode; 2] = [LoopDetectionMode::Off, LoopDetectionMode::Interactive];
const NEWT_LAND: (PlayerId, &str) = (P0, "Island");
const SKYCLAVE_TARGET: (PlayerId, &str) = (P1, "Grizzly Bears");

fn pool(n: usize, mana: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect()
}

/// How `card` enters under P1's Elesh Norn.
#[derive(Clone, Copy)]
enum Entry {
    /// P0 casts Collected Company and puts `card` onto the battlefield.
    CollectedCompany,
    /// P0 casts `card` itself.
    Cast,
}

/// `card` enters under P1's Elesh Norn, where it dies to the state-based
/// action; `exiled` is the permanent its enters trigger exiles.
struct Board {
    runner: GameRunner,
    card: ObjectId,
    exiled: ObjectId,
}

fn board(
    mode: LoopDetectionMode,
    entry: Entry,
    card_name: &str,
    exiled: (PlayerId, &str),
) -> Board {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (card, spell, mana) = match entry {
        Entry::CollectedCompany => {
            let card = scenario.add_real_card(P0, card_name, Zone::Library, db);
            for _ in 0..5 {
                scenario.add_real_card(P0, "Forest", Zone::Library, db);
            }
            let coco = scenario.add_real_card(P0, "Collected Company", Zone::Hand, db);
            let mut mana = pool(1, ManaType::Green);
            mana.extend(pool(3, ManaType::Colorless));
            (card, coco, mana)
        }
        Entry::Cast => {
            let card = scenario.add_real_card(P0, card_name, Zone::Hand, db);
            let mut mana = pool(1, ManaType::Blue);
            mana.extend(pool(1, ManaType::Colorless));
            (card, card, mana)
        }
    };
    scenario.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
    let exiled = scenario.add_real_card(exiled.0, exiled.1, Zone::Battlefield, db);
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    drop(runner.cast(spell).commit());
    Board {
        runner,
        card,
        exiled,
    }
}

fn leaves_description(state: &GameState, card: ObjectId) -> String {
    state.objects[&card]
        .trigger_definitions
        .iter_unchecked()
        .find(|entry| matches!(entry.definition.mode, TriggerMode::LeavesBattlefield))
        .and_then(|entry| entry.definition.description.clone())
        .expect("the card has a described leaves trigger")
}

/// What the run saw on the way to an empty stack.
struct Run {
    runner: GameRunner,
    exiled: ObjectId,
    /// Whether the top stack entry after ordering is the enters exile.
    enters_on_top_after_order: bool,
    /// The exiled object's zone while the leaves trigger is on top.
    exiled_zone_at_leaves: Zone,
    target_prompted: bool,
}

fn run(
    mode: LoopDetectionMode,
    entry: Entry,
    card_name: &str,
    exiled: (PlayerId, &str),
    enters_on_top: bool,
) -> Run {
    let Board {
        mut runner,
        card,
        exiled,
    } = board(mode, entry, card_name, exiled);
    let leaves = leaves_description(runner.state(), card);
    let mut ordered = false;
    let mut enters_on_top_after_order = None;
    let mut exiled_zone_at_leaves = None;
    let mut target_prompted = false;
    for _ in 0..60 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                let top = state.stack.back().map(|entry| &entry.kind);
                if let Some(StackEntryKind::TriggeredAbility {
                    source_id,
                    ability,
                    description,
                    ..
                }) = top
                {
                    if ordered && enters_on_top_after_order.is_none() {
                        enters_on_top_after_order = Some(
                            *source_id == card
                                && matches!(
                                    ability.effect,
                                    Effect::ChangeZone {
                                        destination: Zone::Exile,
                                        ..
                                    }
                                ),
                        );
                    }
                    if *source_id == card && description.as_deref() == Some(leaves.as_str()) {
                        exiled_zone_at_leaves.get_or_insert(state.objects[&exiled].zone);
                    }
                }
                if state.stack.is_empty() {
                    break;
                }
                GameAction::PassPriority
            }
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(*player, P0, "{mode:?}: the controller orders");
                assert_eq!(triggers.len(), 2, "{mode:?}: both triggers wait together");
                assert!(
                    triggers.iter().all(|t| t.source_id == card),
                    "{mode:?}: both are the card's triggers"
                );
                ordered = true;
                // Index 0 is placed first, so the last listed trigger is on top.
                let mut order: Vec<usize> = (0..triggers.len()).collect();
                order.sort_by_key(|&i| (triggers[i].description == leaves) != enters_on_top);
                GameAction::OrderTriggers { order }
            }
            WaitingFor::DigChoice { .. } => GameAction::SelectCards { cards: vec![card] },
            WaitingFor::TriggerTargetSelection { .. } => {
                target_prompted = true;
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(exiled)],
                }
            }
            other => panic!("{mode:?}: unhandled prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    let state = runner.state();
    assert!(state.stack.is_empty(), "{mode:?}: the stack settled");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{mode:?}: ends at priority"
    );
    assert!(ordered, "{mode:?}: the ordering prompt was reached");
    assert_eq!(state.objects[&card].zone, Zone::Graveyard, "{mode:?}");
    Run {
        runner,
        exiled,
        enters_on_top_after_order: enters_on_top_after_order.expect("a trigger was on top"),
        exiled_zone_at_leaves: exiled_zone_at_leaves.expect("the leaves trigger was on top"),
        target_prompted,
    }
}

/// Illusion tokens on the battlefield, as `(controller, power, toughness)`.
fn illusions(state: &GameState) -> Vec<(PlayerId, Option<i32>, Option<i32>)> {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.name == "Illusion")
        .map(|object| (object.controller, object.power, object.toughness))
        .collect()
}

#[test]
fn newt_enters_on_top_returns_the_land_it_exiled() {
    for mode in MODES {
        let r = run(
            mode,
            Entry::CollectedCompany,
            "Wormfang Newt",
            NEWT_LAND,
            true,
        );
        assert!(r.enters_on_top_after_order, "{mode:?}");
        assert_eq!(r.exiled_zone_at_leaves, Zone::Exile, "{mode:?}");
        let island = &r.runner.state().objects[&r.exiled];
        // CR 607.2a: the leaves trigger refers to the card its linked ability exiled.
        assert_eq!(island.zone, Zone::Battlefield, "{mode:?}");
        assert_eq!(island.controller, P0, "{mode:?}");
    }
}

#[test]
fn newt_leaves_on_top_returns_nothing() {
    for mode in MODES {
        let r = run(
            mode,
            Entry::CollectedCompany,
            "Wormfang Newt",
            NEWT_LAND,
            false,
        );
        assert!(!r.enters_on_top_after_order, "{mode:?}");
        assert_eq!(r.exiled_zone_at_leaves, Zone::Battlefield, "{mode:?}");
        assert_eq!(
            r.runner.state().objects[&r.exiled].zone,
            Zone::Exile,
            "{mode:?}"
        );
    }
}

#[test]
fn skyclave_enters_on_top_gives_the_owner_a_token_of_its_mana_value() {
    for mode in MODES {
        let r = run(
            mode,
            Entry::CollectedCompany,
            "Skyclave Apparition",
            SKYCLAVE_TARGET,
            true,
        );
        assert!(r.target_prompted, "{mode:?}");
        assert!(r.enters_on_top_after_order, "{mode:?}");
        assert_eq!(r.exiled_zone_at_leaves, Zone::Exile, "{mode:?}");
        let state = r.runner.state();
        assert_eq!(state.objects[&r.exiled].zone, Zone::Exile, "{mode:?}");
        // CR 607.2a: X is the exiled Bears' mana value 2; P1's Norn adds +2/+2.
        assert_eq!(illusions(state), vec![(P1, Some(4), Some(4))], "{mode:?}");
    }
}

#[test]
fn skyclave_leaves_on_top_creates_no_token() {
    for mode in MODES {
        let r = run(
            mode,
            Entry::CollectedCompany,
            "Skyclave Apparition",
            SKYCLAVE_TARGET,
            false,
        );
        assert!(r.target_prompted, "{mode:?}");
        assert!(!r.enters_on_top_after_order, "{mode:?}");
        assert_eq!(r.exiled_zone_at_leaves, Zone::Battlefield, "{mode:?}");
        let state = r.runner.state();
        assert_eq!(state.objects[&r.exiled].zone, Zone::Exile, "{mode:?}");
        assert!(illusions(state).is_empty(), "{mode:?}");
    }
}

/// CR 603.3b + CR 607.2a: cast directly, Wormfang Newt's enters and leaves
/// triggers wait together for its controller's order.
#[test]
fn cast_newt_enters_on_top_returns_the_land_it_exiled() {
    for mode in MODES {
        let r = run(mode, Entry::Cast, "Wormfang Newt", NEWT_LAND, true);
        assert!(r.enters_on_top_after_order, "{mode:?}");
        assert_eq!(r.exiled_zone_at_leaves, Zone::Exile, "{mode:?}");
        let island = &r.runner.state().objects[&r.exiled];
        assert_eq!(island.zone, Zone::Battlefield, "{mode:?}");
        assert_eq!(island.controller, P0, "{mode:?}");
    }
}

#[test]
fn cast_newt_leaves_on_top_returns_nothing() {
    for mode in MODES {
        let r = run(mode, Entry::Cast, "Wormfang Newt", NEWT_LAND, false);
        assert!(!r.enters_on_top_after_order, "{mode:?}");
        assert_eq!(r.exiled_zone_at_leaves, Zone::Battlefield, "{mode:?}");
        assert_eq!(
            r.runner.state().objects[&r.exiled].zone,
            Zone::Exile,
            "{mode:?}"
        );
    }
}

/// What a row's driver answers at each prompt.
#[derive(Default)]
struct Answers {
    reverse_order: bool,
    /// Target for target prompts; `Player(P1)` when unset.
    target: Option<TargetRef>,
    /// Cards a legend, sacrifice, discard or copy choice prefers.
    pick: Vec<ObjectId>,
    commander_to_command_zone: bool,
    /// The replacement candidate whose description contains this; else index 0.
    replacement: Option<&'static str>,
    accept_optional: bool,
    /// An object whose zone is recorded at every step.
    watch: Option<ObjectId>,
    /// The `ChooseOneOfBranch` branch whose description contains this; else 0.
    branch: Option<&'static str>,
}

struct Prompt {
    player: PlayerId,
    sources: Vec<String>,
    ai_orders: usize,
}

/// A non-priority choice and what was on the stack and in the queue then.
struct Choice {
    kind: &'static str,
    stack: Vec<String>,
    deferred_sources: Vec<String>,
    priorities_before: usize,
}

struct Snapshot {
    stack: Vec<String>,
    controllers: Vec<PlayerId>,
    descriptions: Vec<String>,
    deferred: usize,
}

/// The queue at one step: the open wait, its length, and whether any context
/// serializes a `batch_origin` (only `Ordinary` is elided).
struct QueueStep {
    kind: &'static str,
    deferred: usize,
    origin_tagged: bool,
}

#[derive(Default)]
struct Trace {
    prompts: Vec<Prompt>,
    choices: Vec<Choice>,
    priorities: Vec<Snapshot>,
    watched_zones: Vec<Zone>,
    queue_steps: Vec<QueueStep>,
}

impl Trace {
    fn choice(&self, kind: &str) -> &Choice {
        self.choices
            .iter()
            .find(|choice| choice.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} was opened"))
    }

    /// The first priority after the first `kind` choice.
    fn after(&self, kind: &str) -> &Snapshot {
        &self.priorities[self.choice(kind).priorities_before]
    }

    /// The first priority with abilities on the stack and no spell.
    fn first_ability_stack(&self) -> &Snapshot {
        self.priorities
            .iter()
            .find(|s| !s.stack.is_empty() && !s.stack.iter().any(|x| x.ends_with(SPELL)))
            .expect("a priority with only abilities on the stack")
    }

    fn the_prompt(&self) -> &Prompt {
        assert_eq!(self.prompts.len(), 1, "exactly one ordering prompt");
        &self.prompts[0]
    }

    fn ever_stacked(&self, name: &str) -> bool {
        self.priorities
            .iter()
            .any(|s| s.stack.iter().any(|x| x == name))
    }
}

const SPELL: &str = " (spell)";

fn name_of(state: &GameState, id: ObjectId) -> String {
    state
        .objects
        .get(&id)
        .map(|object| object.name.clone())
        .unwrap_or_default()
}

/// Stack bottom→top: a triggered ability by its source's name, a spell by its
/// name plus `SPELL`.
fn stack_labels(state: &GameState) -> Vec<String> {
    state
        .stack
        .iter()
        .map(|entry| match &entry.kind {
            StackEntryKind::Spell { .. } => format!("{}{SPELL}", name_of(state, entry.source_id)),
            _ => name_of(state, entry.source_id),
        })
        .collect()
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn start(scenario: GameScenario, mode: LoopDetectionMode) -> GameRunner {
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    runner
}

fn picks(cards: &[ObjectId], count: usize, preferred: &[ObjectId]) -> Vec<ObjectId> {
    let mut chosen: Vec<ObjectId> = preferred
        .iter()
        .copied()
        .filter(|card| cards.contains(card))
        .collect();
    chosen.extend(
        cards
            .iter()
            .copied()
            .filter(|card| !preferred.contains(card)),
    );
    chosen.truncate(count);
    chosen
}

/// Drives through `GameRunner::act` until a priority with an empty stack.
fn drive(runner: &mut GameRunner, answers: &Answers) -> Trace {
    let mut trace = Trace::default();
    for _ in 0..120 {
        let state = runner.state();
        if let Some(id) = answers.watch {
            trace.watched_zones.push(state.objects[&id].zone);
        }
        let waiting = state.waiting_for.clone();
        trace.queue_steps.push(QueueStep {
            kind: waiting.variant_name(),
            deferred: state.deferred_triggers.len(),
            origin_tagged: serde_json::to_string(&state.deferred_triggers)
                .expect("queue serializes")
                .contains("batch_origin"),
        });
        if !matches!(
            waiting,
            WaitingFor::Priority { .. }
                | WaitingFor::OrderTriggers { .. }
                | WaitingFor::TriggerTargetSelection { .. }
                | WaitingFor::TargetSelection { .. }
        ) {
            trace.choices.push(Choice {
                kind: waiting.variant_name(),
                stack: stack_labels(state),
                deferred_sources: state
                    .deferred_triggers
                    .iter()
                    .map(|context| name_of(state, context.pending.source_id))
                    .collect(),
                priorities_before: trace.priorities.len(),
            });
        }
        let action = match &waiting {
            WaitingFor::Priority { .. } => {
                trace.priorities.push(Snapshot {
                    stack: stack_labels(state),
                    controllers: state.stack.iter().map(|entry| entry.controller).collect(),
                    descriptions: state
                        .stack
                        .iter()
                        .map(|entry| match &entry.kind {
                            StackEntryKind::TriggeredAbility { description, .. } => {
                                description.clone().unwrap_or_default()
                            }
                            _ => String::new(),
                        })
                        .collect(),
                    deferred: state.deferred_triggers.len(),
                });
                if state.stack.is_empty() {
                    return trace;
                }
                GameAction::PassPriority
            }
            WaitingFor::OrderTriggers { player, triggers } => {
                trace.prompts.push(Prompt {
                    player: *player,
                    sources: triggers.iter().map(|t| t.source_name.clone()).collect(),
                    ai_orders: engine::ai_support::legal_actions(state)
                        .iter()
                        .filter(|a| matches!(a, GameAction::OrderTriggers { .. }))
                        .count(),
                });
                let mut order: Vec<usize> = (0..triggers.len()).collect();
                if answers.reverse_order {
                    order.reverse();
                }
                GameAction::OrderTriggers { order }
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(answers.target.clone().unwrap_or(TargetRef::Player(P1))),
                }
            }
            WaitingFor::ChooseLegend { candidates, .. } => GameAction::ChooseLegend {
                keep: picks(candidates, 1, &answers.pick)[0],
            },
            WaitingFor::CommanderZoneChoice { .. } => GameAction::DecideOptionalEffect {
                accept: answers.commander_to_command_zone,
            },
            WaitingFor::ReplacementChoice { candidates, .. } => GameAction::ChooseReplacement {
                index: answers
                    .replacement
                    .and_then(|wanted| {
                        candidates
                            .iter()
                            .position(|c| c.description.contains(wanted))
                    })
                    .unwrap_or(0),
            },
            WaitingFor::NamedChoice { options, .. } => GameAction::ChooseOption {
                choice: options[0].clone(),
            },
            WaitingFor::EffectZoneChoice { cards, count, .. }
            | WaitingFor::DiscardChoice { cards, count, .. } => GameAction::SelectCards {
                cards: picks(cards, *count, &answers.pick),
            },
            WaitingFor::CopyTargetChoice { valid_targets, .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(picks(valid_targets, 1, &answers.pick)[0])),
            },
            WaitingFor::OptionalEffectChoice { .. } => GameAction::DecideOptionalEffect {
                accept: answers.accept_optional,
            },
            WaitingFor::CastOffer { .. } => GameAction::DecideOptionalEffect { accept: false },
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => GameAction::ChooseBranch {
                index: answers
                    .branch
                    .and_then(|wanted| branch_descriptions.iter().position(|d| d.contains(wanted)))
                    .unwrap_or(0),
            },
            other => panic!("unhandled prompt {}", other.variant_name()),
        };
        runner.act(action).expect("action accepted");
    }
    panic!("the stack never settled");
}

/// CR 603.3b + CR 101.4: Doomed Traveler dies to P1's Elesh Norn as it enters
/// beside P1's Soul Warden; the active player's trigger goes on the stack first.
#[test]
fn cross_controller_batch_stacks_in_apnap_order() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        let traveler = sc.add_real_card(P0, "Doomed Traveler", Zone::Hand, db);
        sc.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
        sc.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
        sc.with_mana_pool(P0, pool(1, ManaType::White));
        let mut r = start(sc, mode);
        drop(r.cast(traveler).commit());
        let trace = drive(&mut r, &Answers::default());
        assert_eq!(
            trace.first_ability_stack().stack,
            names(&["Doomed Traveler", "Soul Warden"]),
            "{mode:?}"
        );
    }
}

/// Emperor Crocodile (optional) and Doomed Traveler; Shock the Traveler.
fn state_trigger_board(mode: LoopDetectionMode, crocodile: bool) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    if crocodile {
        sc.add_real_card(P0, "Emperor Crocodile", Zone::Battlefield, db);
    }
    let traveler = sc.add_real_card(P0, "Doomed Traveler", Zone::Battlefield, db);
    let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
    sc.with_mana_pool(P0, pool(1, ManaType::Red));
    let mut r = start(sc, mode);
    drop(r.cast(shock).target_object(traveler).commit());
    drive(&mut r, &Answers::default())
}

/// CR 603.8 + CR 603.3b: a state trigger true at this boundary joins the dies
/// trigger in one batch.
#[test]
fn state_trigger_joins_the_dies_trigger_batch() {
    for mode in MODES {
        let trace = state_trigger_board(mode, true);
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Doomed Traveler", "Emperor Crocodile"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");

        let alone = state_trigger_board(mode, false);
        assert!(alone.prompts.is_empty(), "{mode:?}");
        assert!(alone.ever_stacked("Doomed Traveler"), "{mode:?}");
    }
}

/// CR 603.8: Pyroclasm kills Doomed Traveler and Blood Artist beside Emperor
/// Crocodile; the state trigger joins the batch once, whatever the order.
#[test]
fn state_trigger_joins_a_batch_with_targeted_members_once() {
    for reverse_order in [false, true] {
        for mode in MODES {
            let db = shared_card_db().expect("card db");
            let mut sc = scenario();
            sc.add_real_card(P0, "Emperor Crocodile", Zone::Battlefield, db);
            sc.add_real_card(P0, "Doomed Traveler", Zone::Battlefield, db);
            sc.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
            let clasm = sc.add_real_card(P0, "Pyroclasm", Zone::Hand, db);
            let mut mana = pool(1, ManaType::Red);
            mana.extend(pool(1, ManaType::Colorless));
            sc.with_mana_pool(P0, mana);
            let mut r = start(sc, mode);
            drop(r.cast(clasm).commit());
            let trace = drive(
                &mut r,
                &Answers {
                    reverse_order,
                    ..Answers::default()
                },
            );
            assert_eq!(
                sorted(trace.the_prompt().sources.clone()),
                names(&[
                    "Blood Artist",
                    "Blood Artist",
                    "Doomed Traveler",
                    "Emperor Crocodile"
                ]),
                "{mode:?} reverse={reverse_order}"
            );
            let most_crocodiles = trace
                .priorities
                .iter()
                .map(|s| s.stack.iter().filter(|x| *x == "Emperor Crocodile").count())
                .max();
            assert_eq!(most_crocodiles, Some(1), "{mode:?} reverse={reverse_order}");
            // CR 603.3b: a placed boundary's re-parked tail is ordinary work.
            assert!(
                trace
                    .queue_steps
                    .iter()
                    .any(|step| step.kind == "TriggerTargetSelection" && step.deferred > 0),
                "{mode:?} reverse={reverse_order}"
            );
            assert!(
                trace.queue_steps.iter().all(|step| !step.origin_tagged),
                "{mode:?} reverse={reverse_order}"
            );
        }
    }
}

/// Banisher Priest exiles P1's Elvish Visionary; Pyroclasm kills the Priest and
/// (optionally) P1's Doomed Traveler, so the Visionary returns.
fn exile_return_board(mode: LoopDetectionMode, traveler: bool) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let priest = sc.add_real_card(P0, "Banisher Priest", Zone::Hand, db);
    let visionary = sc.add_real_card(P1, "Elvish Visionary", Zone::Battlefield, db);
    if traveler {
        sc.add_real_card(P1, "Doomed Traveler", Zone::Battlefield, db);
    }
    let clasm = sc.add_real_card(P0, "Pyroclasm", Zone::Hand, db);
    for _ in 0..2 {
        sc.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut mana = pool(2, ManaType::White);
    mana.extend(pool(1, ManaType::Red));
    mana.extend(pool(2, ManaType::Colorless));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    drop(r.cast(priest).commit());
    drive(
        &mut r,
        &Answers {
            target: Some(TargetRef::Object(visionary)),
            ..Answers::default()
        },
    );
    assert_eq!(r.state().objects[&visionary].zone, Zone::Exile, "{mode:?}");
    drop(r.cast(clasm).commit());
    drive(&mut r, &Answers::default())
}

/// CR 610.3 + CR 603.3b: the returned card's enters trigger joins the dies
/// trigger of the same state-based action in one batch.
#[test]
fn exile_return_trigger_joins_the_boundary_batch() {
    for mode in MODES {
        let trace = exile_return_board(mode, true);
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P1, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Doomed Traveler", "Elvish Visionary"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");

        let alone = exile_return_board(mode, false);
        assert!(alone.prompts.is_empty(), "{mode:?}");
        assert!(alone.ever_stacked("Elvish Visionary"), "{mode:?}");
    }
}

/// CR 704.5j + CR 603.3b: the legend rule opens its choice before any trigger
/// is placed; the answer's dies trigger joins the parked batch.
#[test]
fn legend_rule_answer_joins_the_parked_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        sc.add_real_card(P0, "Aerid Konstrari", Zone::Battlefield, db);
        sc.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
        let second = sc.add_real_card(P0, "Aerid Konstrari", Zone::Hand, db);
        let mut mana = pool(1, ManaType::Red);
        mana.extend(pool(2, ManaType::Green));
        mana.extend(pool(2, ManaType::Colorless));
        sc.with_mana_pool(P0, mana);
        let mut r = start(sc, mode);
        drop(r.cast(second).commit());
        let trace = drive(&mut r, &Answers::default());
        assert!(trace.choice("ChooseLegend").stack.is_empty(), "{mode:?}");
        let prompt = trace.the_prompt();
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Aerid Konstrari", "Aerid Konstrari", "Blood Artist"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 6, "{mode:?}");
    }
}

/// A Good Day to Die (optional) on Doomed Traveler, then Lightning Bolt it.
fn delayed_trigger_board(mode: LoopDetectionMode, good_day: bool) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let traveler = sc.add_real_card(P0, "Doomed Traveler", Zone::Battlefield, db);
    let day = sc.add_real_card(P0, "A Good Day to Die", Zone::Hand, db);
    let bolt = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    for _ in 0..2 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
    }
    let mut mana = pool(1, ManaType::Black);
    mana.extend(pool(1, ManaType::Red));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    if good_day {
        drop(r.cast(day).target_object(traveler).commit());
        drive(&mut r, &Answers::default());
    }
    drop(r.cast(bolt).target_object(traveler).commit());
    drive(&mut r, &Answers::default())
}

/// CR 603.7 + CR 603.3b: a delayed "when that creature dies" trigger joins the
/// creature's own dies trigger in one batch.
#[test]
fn delayed_trigger_joins_the_dies_trigger_batch() {
    for mode in MODES {
        let trace = delayed_trigger_board(mode, true);
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["A Good Day to Die", "Doomed Traveler"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");

        let alone = delayed_trigger_board(mode, false);
        assert!(alone.prompts.is_empty(), "{mode:?}");
        assert!(alone.ever_stacked("Doomed Traveler"), "{mode:?}");
    }
}

/// CR 603.3b: Fleshbag Marauder's sacrifices and the state-based deaths they
/// cause trigger P0's observers as one batch.
#[test]
fn sacrifice_answer_and_sba_deaths_are_one_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        let own_norn = sc.add_real_card(P0, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
        sc.add_real_card(P0, "Doomed Traveler", Zone::Battlefield, db);
        sc.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
        let fleshbag = sc.add_real_card(P0, "Fleshbag Marauder", Zone::Hand, db);
        sc.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
        let bears = sc.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        for _ in 0..3 {
            sc.add_real_card(P0, "Island", Zone::Library, db);
            sc.add_real_card(P1, "Island", Zone::Library, db);
        }
        let mut mana = pool(1, ManaType::Black);
        mana.extend(pool(2, ManaType::Colorless));
        sc.with_mana_pool(P0, mana);
        let mut r = start(sc, mode);
        drop(r.cast(fleshbag).commit());
        let trace = drive(
            &mut r,
            &Answers {
                pick: vec![own_norn, bears],
                ..Answers::default()
            },
        );
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        let mut expected = vec!["Blood Artist"; 5];
        expected.push("Doomed Traveler");
        assert_eq!(sorted(prompt.sources.clone()), names(&expected), "{mode:?}");
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");
    }
}

/// CR 603.3b: P1's discard and the state-based death it causes trigger P0's
/// Waste Not and Blood Artist as one batch.
#[test]
fn discard_answer_and_sba_death_are_one_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        let liliana = sc.add_real_card(P0, "Liliana of the Veil", Zone::Battlefield, db);
        sc.add_real_card(P0, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
        sc.add_real_card(P0, "Waste Not", Zone::Battlefield, db);
        sc.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
        let body = sc.add_real_card(P1, "Body of Knowledge", Zone::Battlefield, db);
        sc.add_real_card(P0, "Island", Zone::Hand, db);
        sc.add_real_card(P0, "Island", Zone::Hand, db);
        let bears = sc.add_real_card(P1, "Grizzly Bears", Zone::Hand, db);
        sc.add_real_card(P1, "Island", Zone::Hand, db);
        sc.add_real_card(P1, "Island", Zone::Hand, db);
        for _ in 0..3 {
            sc.add_real_card(P0, "Island", Zone::Library, db);
            sc.add_real_card(P1, "Island", Zone::Library, db);
        }
        let mut r = start(sc, mode);
        engine::game::rehydrate_game_from_card_db(r.state_mut(), db);
        let plus_one = engine::ai_support::legal_actions(r.state())
            .into_iter()
            .find(|a| {
                matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == liliana)
            })
            .expect("Liliana's +1 is legal");
        r.act(plus_one).expect("activation accepted");
        let trace = drive(
            &mut r,
            &Answers {
                pick: vec![bears],
                ..Answers::default()
            },
        );
        assert_eq!(r.state().objects[&body].zone, Zone::Graveyard, "{mode:?}");
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Blood Artist", "Waste Not"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");
    }
}

/// CR 704.6f: while Mutual Epiphany's encounter trigger has not left the stack
/// the phenomenon stays face up; once it resolves the controller planeswalks.
#[test]
fn phenomenon_stays_until_its_encounter_trigger_leaves_the_stack() {
    use engine::game::deck_loading::create_planar_deck_card;
    use engine::game::planechase::{
        active_plane, planar_ability_sentinel_id, restamp_planar_objects_to_controller,
    };
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        for _ in 0..10 {
            sc.add_real_card(P0, "Island", Zone::Library, db);
            sc.add_real_card(P1, "Island", Zone::Library, db);
        }
        let mut r = start(sc, mode);
        let state = r.state_mut();
        state.format_config = engine::types::format::FormatConfig::planechase();
        let face = |name: &str| db.get_face_by_name(name).expect("planar card");
        let start_plane = create_planar_deck_card(state, face("Academy at Tolaria West"), P0);
        let phenomenon = create_planar_deck_card(state, face("Mutual Epiphany"), P0);
        let next_plane = create_planar_deck_card(state, face("Akoum"), P0);
        state.planar_deck.retain(|&id| id != start_plane);
        state.objects.get_mut(&start_plane).unwrap().face_down = false;
        state.command_zone.push_back(start_plane);
        state.planar_controller = Some(P0);
        restamp_planar_objects_to_controller(state);
        let sentinel = planar_ability_sentinel_id(P0);
        state.stack.push_back(StackEntry {
            id: ObjectId(99_999),
            source_id: sentinel,
            controller: P0,
            kind: StackEntryKind::TriggeredAbility {
                source_id: sentinel,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Planeswalk,
                    vec![],
                    sentinel,
                    P0,
                )),
                condition: None,
                trigger_event: None,
                description: Some("planeswalk".into()),
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        let mut encounter_seen = false;
        for _ in 0..30 {
            let state = r.state();
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "{mode:?}"
            );
            if state.stack.is_empty() {
                break;
            }
            if state
                .stack
                .iter()
                .any(|entry| entry.source_id == phenomenon)
            {
                encounter_seen = true;
                assert_eq!(active_plane(state), Some(phenomenon), "{mode:?}");
            }
            r.act(GameAction::PassPriority).expect("pass accepted");
        }
        assert!(encounter_seen, "{mode:?}");
        assert!(r.state().stack.is_empty(), "{mode:?}");
        assert_eq!(active_plane(r.state()), Some(next_plane), "{mode:?}");
    }
}

/// Clone (P0) entering as a copy of P1's Wormfang Newt under P1's Elesh Norn,
/// with Shock (at P1) below when `spell_below`.
fn clone_copy_board(
    mode: LoopDetectionMode,
    spell_below: bool,
    reverse_order: bool,
) -> (Trace, GameRunner, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let island = sc.add_real_card(P0, "Island", Zone::Battlefield, db);
    let clone = sc.add_real_card(P0, "Clone", Zone::Hand, db);
    sc.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
    let newt = sc.add_real_card(P1, "Wormfang Newt", Zone::Battlefield, db);
    let mut mana = pool(1, ManaType::Blue);
    mana.extend(pool(3, ManaType::Colorless));
    let shock = spell_below.then(|| {
        sc.add_real_card(P0, "Leyline of Anticipation", Zone::Battlefield, db);
        mana.extend(pool(1, ManaType::Red));
        sc.add_real_card(P0, "Shock", Zone::Hand, db)
    });
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    if let Some(shock) = shock {
        drop(r.cast(shock).target_player(P1).commit());
    }
    drop(r.cast(clone).commit());
    let trace = drive(
        &mut r,
        &Answers {
            pick: vec![newt],
            reverse_order,
            ..Answers::default()
        },
    );
    (trace, r, island)
}

/// CR 614.12a + CR 117.5 + CR 603.3b: Clone's copy choice is made before it
/// enters, so the copy's enters trigger and the leaves trigger of its
/// state-based death are one batch, each once (CR 603.2c), in P0's order.
#[test]
fn clone_copy_triggers_share_one_prompt() {
    for mode in MODES {
        let mut enters_on_top_per_answer = Vec::new();
        for reverse in [false, true] {
            let (trace, r, island) = clone_copy_board(mode, false, reverse);
            let prompt = trace.the_prompt();
            assert_eq!(prompt.player, P0, "{mode:?}");
            assert_eq!(
                prompt.sources,
                names(&["Wormfang Newt", "Wormfang Newt"]),
                "{mode:?}"
            );
            assert_eq!(prompt.ai_orders, 2, "{mode:?}");
            let placed = &trace.first_ability_stack().descriptions;
            assert_eq!(placed.len(), 2, "{mode:?}: {placed:?}");
            let enters_on_top = placed[1].contains("enters");
            assert!(
                placed[usize::from(!enters_on_top)].contains("leaves"),
                "{mode:?}: {placed:?}"
            );
            // CR 607.2a: the leaves trigger returns only what the enters trigger exiled.
            let island_zone = if enters_on_top {
                Zone::Battlefield
            } else {
                Zone::Exile
            };
            assert_eq!(r.state().objects[&island].zone, island_zone, "{mode:?}");
            enters_on_top_per_answer.push(enters_on_top);
        }
        assert_ne!(
            enters_on_top_per_answer[0], enters_on_top_per_answer[1],
            "{mode:?}: the answer decides the order"
        );
    }
}

/// CR 117.5 + CR 603.3b + CR 405.5: the same batch goes on the stack above a
/// spell that has not begun to resolve.
#[test]
fn clone_copy_triggers_join_above_a_spell() {
    for mode in MODES {
        let (trace, _, _) = clone_copy_board(mode, true, false);
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(prompt.sources.len(), 2, "{mode:?}");
        let after = trace.after("CopyTargetChoice");
        assert_eq!(
            after.stack,
            names(&["Shock (spell)", "Clone", "Clone"]),
            "{mode:?}"
        );
        assert_eq!(after.deferred, 0, "{mode:?}");
    }
}

/// Sun Titan returns Breeding Pool; paying 2 life drops P0 to 1, so Serra
/// Avatar dies under P1's Elesh Norn beside Kazandu Mammoth's landfall.
fn change_zone_resume_board(mode: LoopDetectionMode, replacement: &'static str) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let titan = sc.add_real_card(P0, "Sun Titan", Zone::Hand, db);
    let land = sc.add_real_card(P0, "Breeding Pool", Zone::Graveyard, db);
    sc.add_real_card(P0, "Kazandu Mammoth", Zone::Battlefield, db);
    let avatar = sc.add_real_card(P0, "Serra Avatar", Zone::Battlefield, db);
    sc.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
    for _ in 0..3 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
    }
    let mut mana = pool(2, ManaType::White);
    mana.extend(pool(4, ManaType::Colorless));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db);
    r.state_mut().players[0].life = 3;
    drop(r.cast(titan).commit());
    let trace = drive(
        &mut r,
        &Answers {
            target: Some(TargetRef::Object(land)),
            replacement: Some(replacement),
            accept_optional: true,
            watch: Some(avatar),
            ..Answers::default()
        },
    );
    assert_eq!(r.state().objects[&land].zone, Zone::Battlefield, "{mode:?}");
    trace
}

/// CR 603.3b: a change-zone resume's landfall trigger joins the state-based
/// death it causes in one batch.
#[test]
fn change_zone_resume_trigger_joins_the_sba_death_batch() {
    for mode in MODES {
        let trace = change_zone_resume_board(mode, "Pay 2 life");
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Kazandu Mammoth", "Serra Avatar"]),
            "{mode:?}"
        );

        let declined = change_zone_resume_board(mode, "Decline");
        assert!(declined.prompts.is_empty(), "{mode:?}");
        assert!(declined.ever_stacked("Kazandu Mammoth"), "{mode:?}");
        assert_eq!(
            declined.watched_zones.last(),
            Some(&Zone::Battlefield),
            "{mode:?}"
        );
    }
}

/// CR 603.8 + CR 608.2n: Innocent Blood's sacrifice puts the dies trigger and
/// Emperor Crocodile's state trigger in one batch on the final-part boundary.
#[test]
fn state_trigger_joins_the_final_part_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        sc.add_real_card(P0, "Emperor Crocodile", Zone::Battlefield, db);
        let traveler = sc.add_real_card(P0, "Doomed Traveler", Zone::Battlefield, db);
        let blood = sc.add_real_card(P0, "Innocent Blood", Zone::Hand, db);
        for _ in 0..3 {
            sc.add_real_card(P0, "Island", Zone::Library, db);
        }
        sc.with_mana_pool(P0, pool(1, ManaType::Black));
        let mut r = start(sc, mode);
        drop(r.cast(blood).commit());
        let trace = drive(
            &mut r,
            &Answers {
                pick: vec![traveler],
                ..Answers::default()
            },
        );
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Doomed Traveler", "Emperor Crocodile"]),
            "{mode:?}"
        );
    }
}

/// Ragavan as P0's commander; command-zone rules on.
fn commander_scenario(db: &engine::database::card_db::CardDatabase) -> (GameScenario, ObjectId) {
    let mut sc = scenario();
    let ragavan = sc.add_real_card(P0, "Ragavan, Nimble Pilferer", Zone::Battlefield, db);
    (sc, ragavan)
}

fn make_commander(r: &mut GameRunner, commander: ObjectId) {
    r.state_mut().format_config.command_zone = true;
    r.state_mut()
        .objects
        .get_mut(&commander)
        .unwrap()
        .is_commander = true;
}

/// CR 903.9a + CR 603.8: after the commander-zone answer, Grave Pact's trigger
/// and Emperor Crocodile's state trigger are one batch.
#[test]
fn state_trigger_joins_the_commander_answer_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let (mut sc, ragavan) = commander_scenario(db);
        sc.add_real_card(P0, "Emperor Crocodile", Zone::Battlefield, db);
        sc.add_real_card(P0, "Grave Pact", Zone::Battlefield, db);
        let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
        sc.with_mana_pool(P0, pool(1, ManaType::Red));
        let mut r = start(sc, mode);
        make_commander(&mut r, ragavan);
        drop(r.cast(shock).target_object(ragavan).commit());
        let trace = drive(
            &mut r,
            &Answers {
                commander_to_command_zone: true,
                ..Answers::default()
            },
        );
        assert_eq!(r.state().objects[&ragavan].zone, Zone::Command, "{mode:?}");
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Emperor Crocodile", "Grave Pact"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");
    }
}

/// CR 704.5v: Invasion of Gobakhan's defeat trigger, parked while the
/// commander choice is open, still spares the Siege from the graveyard.
#[test]
fn parked_siege_trigger_spares_the_siege() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let (mut sc, ragavan) = commander_scenario(db);
        let battle = sc.add_real_card(P0, "Invasion of Gobakhan", Zone::Battlefield, db);
        let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
        let forked = sc.add_real_card(P0, "Forked Bolt", Zone::Hand, db);
        for _ in 0..3 {
            sc.add_real_card(P0, "Island", Zone::Library, db);
        }
        sc.with_mana_pool(P0, pool(2, ManaType::Red));
        let mut r = start(sc, mode);
        make_commander(&mut r, ragavan);
        drop(r.cast(shock).target_object(battle).commit());
        drive(&mut r, &Answers::default());
        drop(
            r.cast(forked)
                .target_objects(&[battle, ragavan])
                .distribute_among(&[
                    (TargetRef::Object(battle), 1),
                    (TargetRef::Object(ragavan), 1),
                ])
                .commit(),
        );
        let trace = drive(
            &mut r,
            &Answers {
                commander_to_command_zone: true,
                watch: Some(battle),
                ..Answers::default()
            },
        );
        let choice = trace.choice("CommanderZoneChoice");
        assert!(
            choice
                .deferred_sources
                .iter()
                .any(|source| source == "Invasion of Gobakhan"),
            "{mode:?}: {:?}",
            choice.deferred_sources
        );
        assert!(
            !trace.watched_zones.contains(&Zone::Graveyard),
            "{mode:?}: {:?}",
            trace.watched_zones
        );
        assert_eq!(r.state().objects[&battle].zone, Zone::Exile, "{mode:?}");
    }
}

/// A Good Day to Die or Death Frenzy, then a second Aerid Konstrari; P0 keeps
/// the new one. Returns the trace and P0's library and life before the cast.
fn legend_delayed_board(mode: LoopDetectionMode, setup: &str) -> (Trace, GameRunner, usize, i32) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let first = sc.add_real_card(P0, "Aerid Konstrari", Zone::Battlefield, db);
    let second = sc.add_real_card(P0, "Aerid Konstrari", Zone::Hand, db);
    let spell = sc.add_real_card(P0, setup, Zone::Hand, db);
    for _ in 0..4 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
    }
    let mut mana = pool(3, ManaType::Black);
    mana.extend(pool(1, ManaType::Red));
    mana.extend(pool(3, ManaType::Green));
    mana.extend(pool(9, ManaType::Colorless));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    let cast = r.cast(spell);
    let cast = if setup == "A Good Day to Die" {
        cast.target_object(first)
    } else {
        cast
    };
    drop(cast.commit());
    drive(&mut r, &Answers::default());
    let library = r.state().players[0].library.len();
    let life = r.state().players[0].life;
    drop(r.cast(second).commit());
    let trace = drive(
        &mut r,
        &Answers {
            pick: vec![second],
            ..Answers::default()
        },
    );
    (trace, r, library, life)
}

/// CR 603.7 + CR 704.5j: a delayed trigger that fires on the legend-rule death
/// joins the answer's batch and resolves once.
#[test]
fn delayed_trigger_joins_the_legend_answer_batch() {
    for mode in MODES {
        let (trace, r, library, _) = legend_delayed_board(mode, "A Good Day to Die");
        assert_eq!(
            sorted(trace.the_prompt().sources.clone()),
            names(&["A Good Day to Die", "Aerid Konstrari", "Aerid Konstrari"]),
            "{mode:?}"
        );
        assert_eq!(r.state().players[0].library.len(), library - 1, "{mode:?}");
    }
}

/// CR 603.7 + CR 611.2c: Death Frenzy's "whenever a creature dies this turn"
/// fires once on the legend-rule death and joins the answer's batch.
#[test]
fn retained_delayed_trigger_joins_the_legend_answer_batch() {
    for mode in MODES {
        let (trace, r, _, life) = legend_delayed_board(mode, "Death Frenzy");
        assert_eq!(
            sorted(trace.the_prompt().sources.clone()),
            names(&["Aerid Konstrari", "Aerid Konstrari", "Death Frenzy"]),
            "{mode:?}"
        );
        assert_eq!(r.state().players[0].life, life + 1, "{mode:?}");
    }
}

/// Sheoldred, Rest in Peace and Leyline of the Void (P0); P1's Elvish
/// Visionary. Electrolyze kills the Visionary and draws, with Shock below when
/// `spell_below`; P1 answers the CR 616.1 choice.
fn replacement_choice_board(mode: LoopDetectionMode, spell_below: bool) -> (Trace, GameRunner) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    sc.add_real_card(P0, "Sheoldred, the Apocalypse", Zone::Battlefield, db);
    sc.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    sc.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    let visionary = sc.add_real_card(P1, "Elvish Visionary", Zone::Battlefield, db);
    let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
    let electrolyze = sc.add_real_card(P0, "Electrolyze", Zone::Hand, db);
    for _ in 0..3 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
        sc.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut mana = pool(3, ManaType::Red);
    mana.extend(pool(1, ManaType::Blue));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    if spell_below {
        drop(r.cast(shock).target_player(P1).commit());
    }
    drop(
        r.cast(electrolyze)
            .target_object(visionary)
            .distribute_among(&[(TargetRef::Object(visionary), 2)])
            .commit(),
    );
    let trace = drive(&mut r, &Answers::default());
    assert_eq!(r.state().objects[&visionary].zone, Zone::Exile, "{mode:?}");
    (trace, r)
}

/// CR 117.5 + CR 603.3b + CR 616.1: the draw trigger waits through P1's
/// replacement choice and goes on the stack before P0's next priority.
#[test]
fn boundary_batch_waits_behind_a_replacement_choice() {
    for mode in MODES {
        let (trace, r) = replacement_choice_board(mode, true);
        assert_eq!(
            trace.choice("ReplacementChoice").stack,
            names(&["Shock (spell)"]),
            "{mode:?}"
        );
        let after = trace.after("ReplacementChoice");
        assert_eq!(
            after.stack,
            names(&["Shock (spell)", "Sheoldred, the Apocalypse"]),
            "{mode:?}"
        );
        assert_eq!(after.deferred, 0, "{mode:?}");
        assert_eq!(r.state().players[0].life, 22, "{mode:?}");

        let (control, _) = replacement_choice_board(mode, false);
        let after = control.after("ReplacementChoice");
        assert_eq!(
            after.stack,
            names(&["Sheoldred, the Apocalypse"]),
            "{mode:?}"
        );
        assert_eq!(after.deferred, 0, "{mode:?}");
    }
}

/// CR 903.9a + CR 603.3b: declining the commander-zone choice opens no event,
/// yet the parked Grave Pact trigger goes on the stack before P0's priority.
#[test]
fn declined_commander_choice_places_the_parked_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let (mut sc, ragavan) = commander_scenario(db);
        sc.add_real_card(P0, "Grave Pact", Zone::Battlefield, db);
        let bears = sc.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
        let bolt = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
        for _ in 0..3 {
            sc.add_real_card(P0, "Island", Zone::Library, db);
            sc.add_real_card(P1, "Island", Zone::Library, db);
        }
        sc.with_mana_pool(P0, pool(2, ManaType::Red));
        let mut r = start(sc, mode);
        make_commander(&mut r, ragavan);
        drop(r.cast(shock).target_player(P1).commit());
        drop(r.cast(bolt).target_object(ragavan).commit());
        let trace = drive(&mut r, &Answers::default());
        let after = trace.after("CommanderZoneChoice");
        assert_eq!(
            after.stack,
            names(&["Shock (spell)", "Grave Pact"]),
            "{mode:?}"
        );
        assert_eq!(after.deferred, 0, "{mode:?}");
        assert_eq!(r.state().objects[&bears].zone, Zone::Graveyard, "{mode:?}");
    }
}

/// Banisher Priest exiles P1's Adaptive Automaton; Lightning Bolt kills the
/// Priest (Shock below when `spell_below`), and the Automaton returns into its
/// as-enters creature-type choice while Grave Pact's trigger waits. P0 also
/// controls Soul Warden when `warden`.
fn exit_park_board(mode: LoopDetectionMode, spell_below: bool, warden: bool) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let priest = sc.add_real_card(P0, "Banisher Priest", Zone::Hand, db);
    sc.add_real_card(P0, "Grave Pact", Zone::Battlefield, db);
    if warden {
        sc.add_real_card(P0, "Soul Warden", Zone::Battlefield, db);
    }
    let automaton = sc.add_real_card(P1, "Adaptive Automaton", Zone::Battlefield, db);
    sc.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
    let bolt = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    for _ in 0..3 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
        sc.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut mana = pool(2, ManaType::White);
    mana.extend(pool(2, ManaType::Red));
    mana.extend(pool(1, ManaType::Colorless));
    sc.with_mana_pool(P0, mana);
    // Setup: placing the Automaton abandons its as-enters choice and hands
    // priority to P1; the opening window is P0's.
    sc.at_phase(Phase::PreCombatMain);
    let mut r = start(sc, mode);
    drop(r.cast(priest).commit());
    drive(
        &mut r,
        &Answers {
            target: Some(TargetRef::Object(automaton)),
            ..Answers::default()
        },
    );
    assert_eq!(r.state().objects[&automaton].zone, Zone::Exile, "{mode:?}");
    if spell_below {
        drop(r.cast(shock).target_player(P1).commit());
    }
    drop(r.cast(bolt).target_object(priest).commit());
    drive(&mut r, &Answers::default())
}

/// CR 610.3 + CR 117.5 + CR 603.3b: Grave Pact's trigger from the Priest's
/// death waits through the returned card's as-enters choice and goes on the
/// stack before P0's next priority.
#[test]
fn boundary_batch_waits_behind_an_exile_return_choice() {
    for mode in MODES {
        let trace = exit_park_board(mode, true, false);
        assert_eq!(
            trace.choice("NamedChoice").stack,
            names(&["Shock (spell)"]),
            "{mode:?}"
        );
        let after = trace.after("NamedChoice");
        assert_eq!(
            after.stack,
            names(&["Shock (spell)", "Grave Pact"]),
            "{mode:?}"
        );
        assert_eq!(after.deferred, 0, "{mode:?}");

        let control = exit_park_board(mode, false, false);
        let after = control.after("NamedChoice");
        assert_eq!(after.stack, names(&["Grave Pact"]), "{mode:?}");
        assert_eq!(after.deferred, 0, "{mode:?}");
    }
}

/// Sheoldred, Rest in Peace, Leyline of the Void and Wormfang Newt (P0); P1's
/// Elvish Visionary. Fiery Cannonade kills both (Shock below when
/// `spell_below`); the Visionary's death opens P1's CR 616.1 choice.
fn break_park_board(mode: LoopDetectionMode, spell_below: bool) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    sc.add_real_card(P0, "Sheoldred, the Apocalypse", Zone::Battlefield, db);
    sc.add_real_card(P0, "Rest in Peace", Zone::Battlefield, db);
    sc.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    let newt = sc.add_real_card(P0, "Wormfang Newt", Zone::Battlefield, db);
    let visionary = sc.add_real_card(P1, "Elvish Visionary", Zone::Battlefield, db);
    let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
    let cannonade = sc.add_real_card(P0, "Fiery Cannonade", Zone::Hand, db);
    for _ in 0..3 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
        sc.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut mana = pool(2, ManaType::Red);
    mana.extend(pool(2, ManaType::Colorless));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    if spell_below {
        drop(r.cast(shock).target_player(P1).commit());
    }
    drop(r.cast(cannonade).commit());
    let trace = drive(&mut r, &Answers::default());
    assert_eq!(r.state().objects[&newt].zone, Zone::Exile, "{mode:?}");
    assert_eq!(r.state().objects[&visionary].zone, Zone::Exile, "{mode:?}");
    trace
}

/// CR 117.5 + CR 603.3b: the leaves trigger from the same state-based event
/// that opened P1's replacement choice is this boundary's batch.
#[test]
fn sba_event_trigger_waits_behind_a_replacement_choice() {
    for mode in MODES {
        let trace = break_park_board(mode, true);
        let after = trace.after("ReplacementChoice");
        assert_eq!(
            after.stack,
            names(&["Shock (spell)", "Wormfang Newt"]),
            "{mode:?}"
        );
        assert_eq!(after.deferred, 0, "{mode:?}");

        let control = break_park_board(mode, false);
        let after = control.after("ReplacementChoice");
        assert_eq!(after.stack, names(&["Wormfang Newt"]), "{mode:?}");
        assert_eq!(after.deferred, 0, "{mode:?}");
    }
}

fn life(r: &GameRunner, player: PlayerId) -> i32 {
    r.state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player")
        .life
}

fn eliminated(r: &GameRunner, player: PlayerId) -> bool {
    r.state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player")
        .is_eliminated
}

/// Three players; P1 at 1 life controls Sheoldred. P0's Electrolyze deals 1 to
/// P1 and 1 to P2's Grizzly Bears, or, with `p0_sheoldred`, to P0's Doomed
/// Traveler beside P0's own Sheoldred; then P0 draws.
fn elimination_board(mode: LoopDetectionMode, p0_sheoldred: bool) -> (Trace, GameRunner) {
    let db = shared_card_db().expect("card db");
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P1, 1);
    sc.add_real_card(P1, "Sheoldred, the Apocalypse", Zone::Battlefield, db);
    let second = if p0_sheoldred {
        sc.add_real_card(P0, "Sheoldred, the Apocalypse", Zone::Battlefield, db);
        sc.add_real_card(P0, "Doomed Traveler", Zone::Battlefield, db)
    } else {
        sc.add_real_card(PlayerId(2), "Grizzly Bears", Zone::Battlefield, db)
    };
    let electrolyze = sc.add_real_card(P0, "Electrolyze", Zone::Hand, db);
    for _ in 0..3 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
    }
    let mut mana = pool(1, ManaType::Red);
    mana.extend(pool(1, ManaType::Blue));
    mana.extend(pool(1, ManaType::Colorless));
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    drop(
        r.cast(electrolyze)
            .target_player(P1)
            .target_object(second)
            .distribute_among(&[(TargetRef::Player(P1), 1), (TargetRef::Object(second), 1)])
            .commit(),
    );
    let trace = drive(&mut r, &Answers::default());
    (trace, r)
}

/// CR 800.4a + CR 800.4d: P1 leaves the game in the state-based actions after
/// P0's draw, so P1's Sheoldred trigger from that draw is never put on the stack.
#[test]
fn eliminated_players_trigger_never_reaches_the_stack() {
    for mode in MODES {
        let (trace, r) = elimination_board(mode, false);
        assert!(eliminated(&r, P1), "{mode:?}");
        let p0 = r.state().players.iter().find(|p| p.id == P0).expect("P0");
        assert!(
            p0.hand.iter().any(|id| name_of(r.state(), *id) == "Island"),
            "{mode:?}"
        );
        assert!(!trace.ever_stacked("Sheoldred, the Apocalypse"), "{mode:?}");
        assert_eq!(life(&r, P0), 20, "{mode:?}");
    }
}

/// CR 603.3b + CR 800.4d: P0's draw and Doomed Traveler's death triggered with
/// no priority between them, so they are one P0 batch while P1, eliminated in
/// the same state-based actions, places nothing.
#[test]
fn elimination_keeps_the_survivors_batch_whole() {
    for mode in MODES {
        let (trace, r) = elimination_board(mode, true);
        assert!(eliminated(&r, P1), "{mode:?}");
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Doomed Traveler", "Sheoldred, the Apocalypse"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 2, "{mode:?}");
        assert!(
            trace
                .priorities
                .iter()
                .all(|s| !s.controllers.contains(&P1)),
            "{mode:?}"
        );
        assert_eq!(life(&r, P0), 22, "{mode:?}");
    }
}

/// P1's Soul Warden and Blood Artist under P1's Elesh Norn; P0's Denry Klin
/// enters with a first strike counter and dies to the state-based action.
fn denry_board(mode: LoopDetectionMode, spell_below: bool) -> Trace {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario();
    let denry = sc.add_real_card(P0, "Denry Klin, Editor in Chief", Zone::Hand, db);
    sc.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
    sc.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    sc.add_real_card(P1, "Blood Artist", Zone::Battlefield, db);
    let mut mana = pool(1, ManaType::White);
    mana.extend(pool(1, ManaType::Blue));
    mana.extend(pool(2, ManaType::Colorless));
    let shock = spell_below.then(|| {
        sc.add_real_card(P0, "Leyline of Anticipation", Zone::Battlefield, db);
        mana.extend(pool(1, ManaType::Red));
        sc.add_real_card(P0, "Shock", Zone::Hand, db)
    });
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    if let Some(shock) = shock {
        drop(r.cast(shock).target_player(P1).commit());
    }
    drop(r.cast(denry).commit());
    drive(
        &mut r,
        &Answers {
            branch: Some("first strike"),
            ..Answers::default()
        },
    )
}

/// CR 614.12a + CR 603.3b + CR 101.4: Denry Klin's enters observers and the
/// dies observer of its state-based death are one batch: P0's trigger first,
/// then P1 orders its two, above any spell below.
#[test]
fn enters_with_counter_choice_joins_the_dies_batch() {
    for mode in MODES {
        for spell_below in [false, true] {
            let trace = denry_board(mode, spell_below);
            let prompt = trace.the_prompt();
            assert_eq!(prompt.player, P1, "{mode:?} {spell_below}");
            assert_eq!(
                sorted(prompt.sources.clone()),
                names(&["Blood Artist", "Soul Warden"]),
                "{mode:?} {spell_below}"
            );
            let first = trace.after("ChooseOneOfBranch");
            let base = usize::from(spell_below);
            if spell_below {
                assert_eq!(first.stack[0], "Shock (spell)", "{mode:?}");
            }
            assert_eq!(
                first.stack[base], "Denry Klin, Editor in Chief",
                "{mode:?} {spell_below}"
            );
            assert_eq!(
                first.controllers[base..],
                [P0, P1, P1],
                "{mode:?} {spell_below}"
            );
        }
    }
}

/// CR 608.2n + CR 603.3b: Zombify returns Clone, which copies P1's Newt under
/// P1's Elesh Norn; the copy's batch goes above the Shock below Zombify.
#[test]
fn reanimated_clone_joins_the_final_part_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        sc.add_real_card(P0, "Island", Zone::Battlefield, db);
        sc.add_real_card(P0, "Leyline of Anticipation", Zone::Battlefield, db);
        let clone = sc.add_real_card(P0, "Clone", Zone::Graveyard, db);
        let zombify = sc.add_real_card(P0, "Zombify", Zone::Hand, db);
        let shock = sc.add_real_card(P0, "Shock", Zone::Hand, db);
        sc.add_real_card(P1, "Elesh Norn, Grand Cenobite", Zone::Battlefield, db);
        let newt = sc.add_real_card(P1, "Wormfang Newt", Zone::Battlefield, db);
        let mut mana = pool(1, ManaType::Black);
        mana.extend(pool(1, ManaType::Red));
        mana.extend(pool(3, ManaType::Colorless));
        sc.with_mana_pool(P0, mana);
        let mut r = start(sc, mode);
        drop(r.cast(shock).target_player(P1).commit());
        drop(r.cast(zombify).target_object(clone).commit());
        let trace = drive(
            &mut r,
            &Answers {
                pick: vec![newt],
                ..Answers::default()
            },
        );
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(prompt.sources.len(), 2, "{mode:?}");
        assert_eq!(
            trace.after("CopyTargetChoice").stack,
            names(&["Shock (spell)", "Clone", "Clone"]),
            "{mode:?}"
        );
    }
}

/// CR 704.5j + CR 614.12a + CR 603.3b: Clone copying P0's Aerid Konstrari
/// opens the legend rule; the copy's enters trigger waits through the choice
/// and joins the dies triggers of its answer.
#[test]
fn clone_copying_a_legend_joins_the_legend_answer_batch() {
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = scenario();
        let aerid = sc.add_real_card(P0, "Aerid Konstrari", Zone::Battlefield, db);
        sc.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
        let clone = sc.add_real_card(P0, "Clone", Zone::Hand, db);
        let mut mana = pool(1, ManaType::Blue);
        mana.extend(pool(3, ManaType::Colorless));
        sc.with_mana_pool(P0, mana);
        let mut r = start(sc, mode);
        drop(r.cast(clone).commit());
        let trace = drive(
            &mut r,
            &Answers {
                pick: vec![aerid],
                ..Answers::default()
            },
        );
        assert!(trace.choice("ChooseLegend").stack.is_empty(), "{mode:?}");
        let prompt = trace.the_prompt();
        assert_eq!(prompt.player, P0, "{mode:?}");
        assert_eq!(
            sorted(prompt.sources.clone()),
            names(&["Aerid Konstrari", "Aerid Konstrari", "Blood Artist"]),
            "{mode:?}"
        );
        assert_eq!(prompt.ai_orders, 6, "{mode:?}");
    }
}

/// CR 610.3 + CR 603.3b: Soul Warden's trigger from the returned Automaton's
/// entry and Grave Pact's from the Priest's death are one P0 batch, above any
/// spell below.
#[test]
fn exile_return_observer_joins_the_boundary_batch() {
    for mode in MODES {
        for spell_below in [false, true] {
            let trace = exit_park_board(mode, spell_below, true);
            let prompt = trace.the_prompt();
            assert_eq!(prompt.player, P0, "{mode:?} {spell_below}");
            assert_eq!(
                sorted(prompt.sources.clone()),
                names(&["Grave Pact", "Soul Warden"]),
                "{mode:?} {spell_below}"
            );
            if spell_below {
                let after = trace.after("NamedChoice");
                assert_eq!(after.stack[0], "Shock (spell)", "{mode:?}");
                assert_eq!(
                    sorted(after.stack[1..].to_vec()),
                    names(&["Grave Pact", "Soul Warden"]),
                    "{mode:?}"
                );
                assert_eq!(after.deferred, 0, "{mode:?}");
            }
        }
    }
}

/// Syphon Mind in a three-player game; P0 controls Waste Not and Megrim; each
/// opponent holds Lightning Bolt and Shock. P0's Shock at P1 is below when
/// `spell_below`.
fn syphon_board(mode: LoopDetectionMode, spell_below: bool) -> (Trace, GameRunner) {
    let db = shared_card_db().expect("card db");
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    let syphon = sc.add_real_card(P0, "Syphon Mind", Zone::Hand, db);
    sc.add_real_card(P0, "Waste Not", Zone::Battlefield, db);
    sc.add_real_card(P0, "Megrim", Zone::Battlefield, db);
    for opponent in [P1, PlayerId(2)] {
        sc.add_real_card(opponent, "Lightning Bolt", Zone::Hand, db);
        sc.add_real_card(opponent, "Shock", Zone::Hand, db);
    }
    for _ in 0..6 {
        sc.add_real_card(P0, "Island", Zone::Library, db);
    }
    let mut mana = pool(1, ManaType::Black);
    mana.extend(pool(3, ManaType::Colorless));
    let shock = spell_below.then(|| {
        sc.add_real_card(P0, "Leyline of Anticipation", Zone::Battlefield, db);
        mana.extend(pool(1, ManaType::Red));
        sc.add_real_card(P0, "Shock", Zone::Hand, db)
    });
    sc.with_mana_pool(P0, mana);
    let mut r = start(sc, mode);
    if let Some(shock) = shock {
        drop(r.cast(shock).target_player(P1).commit());
    }
    drop(r.cast(syphon).commit());
    let trace = drive(&mut r, &Answers::default());
    (trace, r)
}

/// CR 608.2 + CR 603.3 + CR 603.3b (issue #1793): no player receives priority
/// while Syphon Mind resolves, so its four discard triggers wait and go on the
/// stack together afterwards, P0 ordering them, above any spell below.
#[test]
fn syphon_mind_orders_its_batch_after_resolution() {
    for mode in MODES {
        for spell_below in [false, true] {
            let (trace, r) = syphon_board(mode, spell_below);
            let last_discard = trace
                .queue_steps
                .iter()
                .rposition(|step| step.kind == "DiscardChoice")
                .expect("a discard choice");
            assert!(
                !trace.queue_steps[..last_discard]
                    .iter()
                    .any(|step| step.kind == "OrderTriggers"),
                "{mode:?} {spell_below}: ordering offered mid-resolution"
            );
            let prompt = trace.the_prompt();
            assert_eq!(prompt.player, P0, "{mode:?} {spell_below}");
            assert_eq!(
                sorted(prompt.sources.clone()),
                names(&["Megrim", "Megrim", "Waste Not", "Waste Not"]),
                "{mode:?} {spell_below}"
            );
            assert_eq!(prompt.ai_orders, 24, "{mode:?} {spell_below}");
            assert_eq!(r.state().players[0].hand.len(), 4, "{mode:?} {spell_below}");
            let p1_life = if spell_below { 16 } else { 18 };
            assert_eq!(life(&r, P1), p1_life, "{mode:?} {spell_below}");
            assert_eq!(life(&r, PlayerId(2)), 18, "{mode:?} {spell_below}");
            if spell_below {
                let discard = trace
                    .choices
                    .iter()
                    .rfind(|choice| choice.kind == "DiscardChoice")
                    .expect("a discard choice");
                let after = &trace.priorities[discard.priorities_before];
                assert_eq!(after.stack[0], "Shock (spell)", "{mode:?}");
                assert_eq!(
                    sorted(after.stack[1..].to_vec()),
                    names(&["Megrim", "Megrim", "Waste Not", "Waste Not"]),
                    "{mode:?}"
                );
            }
        }
    }
}

/// CR 800.4a + CR 603.3b + CR 101.4: P1 concedes while P2 is the walk's chooser, so the P1-owned
/// Grizzly Bears P0 controls leaves and Ninja Teen's trigger waits with the walk. Once the walk's
/// sacrifice settles, that trigger and the sacrifice's are put on the stack as one batch.
#[test]
fn a_concession_during_a_keep_walk_joins_the_walks_batch() {
    type Board = fn(&mut GameScenario) -> (&'static str, Vec<ManaUnit>, Vec<&'static str>);
    let gearhulk: Board = |sc| {
        let db = shared_card_db().expect("card db");
        sc.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        sc.add_real_card(P0, "Zulaport Cutthroat", Zone::Battlefield, db);
        (
            "Cataclysmic Gearhulk",
            pool(5, ManaType::White),
            vec!["Cataclysmic Gearhulk", "Zulaport Cutthroat", "Ninja Teen"],
        )
    };
    let limited_resources: Board = |sc| {
        let db = shared_card_db().expect("card db");
        sc.add_real_card(P0, "Countryside Crusher", Zone::Battlefield, db);
        sc.add_real_card(P0, "Titania, Voice of Gaea", Zone::Battlefield, db);
        for player in [P0, P1, PlayerId(2)] {
            for _ in 0..7 {
                sc.add_real_card(player, "Forest", Zone::Battlefield, db);
            }
        }
        ("Limited Resources", pool(1, ManaType::White), vec![])
    };
    for (board, expected) in [
        (
            gearhulk,
            names(&[
                "Ninja Teen",
                "Ninja Teen",
                "Ninja Teen",
                "Zulaport Cutthroat",
                "Zulaport Cutthroat",
            ]),
        ),
        (
            limited_resources,
            names(&[
                "Countryside Crusher",
                "Countryside Crusher",
                "Ninja Teen",
                "Titania, Voice of Gaea",
            ]),
        ),
    ] {
        for mode in MODES {
            let db = shared_card_db().expect("card db");
            let mut sc = GameScenario::new_n_player(3, 9691);
            sc.at_phase(Phase::PreCombatMain);
            sc.add_real_card(P0, "Ninja Teen", Zone::Battlefield, db);
            sc.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
            let stolen = sc.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
            for player in [P1, PlayerId(2)] {
                sc.add_real_card(player, "Grizzly Bears", Zone::Battlefield, db);
                sc.add_real_card(player, "Grizzly Bears", Zone::Battlefield, db);
            }
            let (source, mana, keep_names) = board(&mut sc);
            let spell = sc.add_real_card(P0, source, Zone::Hand, db);
            sc.with_mana_pool(P0, mana);
            let mut r = start(sc, mode);
            let bears = r.state_mut().objects.get_mut(&stolen).unwrap();
            bears.base_controller = Some(P0);
            bears.controller = P0;
            let keep: Vec<ObjectId> = r
                .state()
                .objects
                .values()
                .filter(|obj| obj.controller == P0 && keep_names.contains(&obj.name.as_str()))
                .map(|obj| obj.id)
                .chain(std::iter::once(spell))
                .collect();
            drop(r.cast(spell).commit());

            let mut conceded = false;
            let settled = loop {
                let waiting = r.state().waiting_for.clone();
                let action = match &waiting {
                    WaitingFor::Priority { .. } => GameAction::PassPriority,
                    WaitingFor::CategoryChoice { player, .. }
                    | WaitingFor::KeepExactPermanentsChoice { player, .. }
                        if *player == PlayerId(2) && !conceded =>
                    {
                        r.act(GameAction::Concede { player_id: P1 })
                            .expect("P1 concedes");
                        conceded = true;
                        assert_eq!(
                            r.state().waiting_for,
                            waiting,
                            "{source} {mode:?}: P2 is still choosing"
                        );
                        assert_eq!(
                            r.state()
                                .deferred_triggers
                                .iter()
                                .map(|context| name_of(r.state(), context.pending.source_id))
                                .collect::<Vec<_>>(),
                            names(&["Ninja Teen"]),
                            "{source} {mode:?}: reach: Ninja Teen's trigger waits with the walk"
                        );
                        continue;
                    }
                    WaitingFor::CategoryChoice {
                        eligible_per_category,
                        ..
                    } => {
                        let mut chosen: Vec<ObjectId> = Vec::new();
                        let choices = eligible_per_category
                            .iter()
                            .map(|eligible| {
                                let open: Vec<ObjectId> = eligible
                                    .iter()
                                    .copied()
                                    .filter(|id| !chosen.contains(id))
                                    .collect();
                                let pick = picks(&open, 1, &keep).first().copied();
                                chosen.extend(pick);
                                pick
                            })
                            .collect();
                        GameAction::SelectCategoryPermanents { choices }
                    }
                    WaitingFor::KeepExactPermanentsChoice {
                        eligible,
                        required_count,
                        ..
                    } => GameAction::ChooseKeptPermanents {
                        kept: picks(eligible, *required_count, &keep),
                    },
                    _ => break waiting,
                };
                let answered_last = conceded && !matches!(waiting, WaitingFor::Priority { .. });
                r.act(action).expect("action accepted");
                if answered_last {
                    break r.state().waiting_for.clone();
                }
            };
            let WaitingFor::OrderTriggers { player, triggers } = settled else {
                panic!("{source} {mode:?}: expected one ordering prompt, got {settled:?}");
            };
            assert_eq!(player, P0, "{source} {mode:?}");
            assert_eq!(
                sorted(triggers.iter().map(|t| t.source_name.clone()).collect()),
                expected,
                "{source} {mode:?}"
            );
        }
    }
}

/// CR 704.5a + CR 800.4a + CR 603.3b: P2 at 5 life takes Browbeat's 5, so Exquisite Blood
/// triggers while Browbeat resolves; P2's loss then takes the P2-owned Grizzly Bears P0 controls
/// and Ninja Teen triggers. The two are put on the stack as one batch.
#[test]
fn a_loss_after_a_paused_spell_joins_its_batch() {
    let p2 = PlayerId(2);
    for mode in MODES {
        let db = shared_card_db().expect("card db");
        let mut sc = GameScenario::new_n_player(3, 9691);
        sc.at_phase(Phase::PreCombatMain);
        sc.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
        sc.add_real_card(P0, "Ninja Teen", Zone::Battlefield, db);
        let stolen = sc.add_real_card(p2, "Grizzly Bears", Zone::Battlefield, db);
        sc.with_life(p2, 5);
        let browbeat = sc.add_real_card(P0, "Browbeat", Zone::Hand, db);
        sc.with_mana_pool(P0, pool(3, ManaType::Red));
        let mut r = start(sc, mode);
        let bears = r.state_mut().objects.get_mut(&stolen).unwrap();
        bears.base_controller = Some(P0);
        bears.controller = P0;
        drop(r.cast(browbeat).target_player(P1).commit());

        let settled = loop {
            let waiting = r.state().waiting_for.clone();
            let action = match &waiting {
                WaitingFor::Priority { .. }
                    if stack_labels(r.state()).iter().any(|x| x.ends_with(SPELL)) =>
                {
                    GameAction::PassPriority
                }
                WaitingFor::OpponentMayChoice { player, .. } => GameAction::DecideOptionalEffect {
                    accept: *player == p2,
                },
                _ => break waiting,
            };
            r.act(action).expect("action accepted");
        };
        assert!(eliminated(&r, p2), "{mode:?}: reach");
        let WaitingFor::OrderTriggers { player, triggers } = settled else {
            panic!(
                "{mode:?}: expected one ordering prompt, got {settled:?} over {:?}",
                stack_labels(r.state())
            );
        };
        assert_eq!(player, P0, "{mode:?}");
        assert_eq!(
            sorted(triggers.iter().map(|t| t.source_name.clone()).collect()),
            names(&["Exquisite Blood", "Ninja Teen"]),
            "{mode:?}"
        );
    }
}
