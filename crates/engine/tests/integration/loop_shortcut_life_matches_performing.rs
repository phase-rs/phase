//! CR 732.2c + CR 119.3: a taken shortcut leaves every life total, shared total and per-turn life
//! counter where declining it and performing the same cycles leaves them. (One board of the class
//! is known to differ and is not compared: a Two-Headed Giant drain whose take would end the game;
//! see `giant_specs`.) One oracle for the whole class: each board is driven to its shortcut offer
//! once, the state at the offer is copied, one copy takes `Fixed(n)` and the other declines and
//! performs `n` cycles by hand, and everything a rules reader can read about life is then compared
//! for every seat (stored field, shared total, gained and lost counters, who has left the game, how
//! the game stands), right after the take and again after each later reader: Ancient Tomb ("{T}:
//! Add {C}{C}. This land deals 2 damage to you."), Children of Korlis ("Sacrifice this creature:
//! You gain life equal to the life you've lost this turn."), and the end step. The battlefield is
//! compared too (controller, name, token, power, toughness, +1/+1 counters), for the readers whose
//! body changes with life.
//!
//! The loop is Gravecrawler returned from the graveyard and sacrificed to Phyrexian Altar, with
//! the payoffs each family names. Across the families, the rows put a life total at an ordinary
//! value and near and at `i32::MAX` (and, in `loss`, a drained total near and at `i32::MIN`), and
//! a gained or lost counter near and at its pin, so that a performed take is compared on either
//! side of every limit the engine saturates at, and a batched take, which only gains and passes
//! no life limit, below and onto `i32::MAX` with the gained counter short of, crossing and at its
//! pin.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::database::card_db::CardDatabase;
use engine::game::players::{self, team_life_sum};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, PayableResource, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::loop_period_remade_token_source::markers;
use crate::play_trace;

const P2: PlayerId = PlayerId(2);
const MAX: i32 = i32::MAX;
const MIN: i32 = i32::MIN;
const PIN: u32 = u32::MAX;

/// Children of Korlis' verbatim Oracle text (not a member of the test fixture).
const CHILDREN_OF_KORLIS: &str = "Sacrifice this creature: You gain life equal to the life you've \
                                  lost this turn. (Damage causes loss of life.)";

/// Serra Ascendant's verbatim Oracle text (not a member of the test fixture).
const SERRA_ASCENDANT: &str = "Lifelink (Damage dealt by this creature also causes you to gain \
                               that much life.)\nAs long as you have 30 or more life, this \
                               creature gets +5/+5 and has flying.";

/// A card on a board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Card {
    /// A member of the test fixture, by name.
    Printed(&'static str),
    /// A printed creature that is not a fixture member: name, power, toughness, Oracle text.
    Creature(&'static str, i32, i32, &'static str),
}

impl Card {
    fn name(self) -> &'static str {
        match self {
            Card::Printed(name) | Card::Creature(name, ..) => name,
        }
    }
}

const WARDEN: Card = Card::Printed("Soul Warden");
const CARNIVAL: Card = Card::Printed("Carnival of Souls");
const SERRA: Card = Card::Creature("Serra Ascendant", 1, 1, SERRA_ASCENDANT);

/// How the board's creature returns from the graveyard to be sacrificed each cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    /// Gravecrawler cast with the Phyrexian Altar's {B}: the return and sacrifice net no mana (a
    /// payoff such as Carnival of Souls adds its {B}).
    Black,
    /// Gravecrawler cast with Birds of Paradise, untapped by Intruder Alarm, so the Phyrexian
    /// Altar's {W} is net: the loop stays on offer where a limit holds its life.
    White,
}

/// One board and the count taken on it.
#[derive(Clone, Debug)]
struct Spec {
    two_headed_giant: bool,
    engine: Engine,
    payoffs: Vec<(PlayerId, Card)>,
    /// Each seat's stored life field.
    life: Vec<i32>,
    /// Each seat's gained and lost counters at the start.
    gained: Vec<u32>,
    lost: Vec<u32>,
    cycles: u32,
    /// How many shortcuts are taken in the step, each of `cycles`, with the cycles performed by
    /// hand between them that it takes to be offered the next.
    takes: u32,
}

fn near(value: i64) -> String {
    let (max, min) = (i64::from(MAX), i64::from(MIN));
    if value > max - 1000 {
        match max - value {
            0 => "MAX".into(),
            d => format!("MAX-{d}"),
        }
    } else if value < min + 1000 {
        match value - min {
            0 => "MIN".into(),
            d => format!("MIN+{d}"),
        }
    } else {
        value.to_string()
    }
}

fn near_u(value: u32) -> String {
    if value > PIN - 1000 {
        match PIN - value {
            0 => "PIN".into(),
            d => format!("PIN-{d}"),
        }
    } else {
        value.to_string()
    }
}

impl Spec {
    fn new(two_headed_giant: bool, payoffs: &[(PlayerId, Card)]) -> Self {
        let seats = if two_headed_giant { 4 } else { 2 };
        Spec {
            two_headed_giant,
            engine: Engine::Black,
            payoffs: payoffs.to_vec(),
            life: vec![20; seats],
            gained: vec![0; seats],
            lost: vec![0; seats],
            cycles: 3,
            takes: 1,
        }
    }

    fn label(&self) -> String {
        let payoffs: Vec<String> = self
            .payoffs
            .iter()
            .map(|(seat, card)| format!("P{}:{}", seat.0, card.name()))
            .collect();
        let life: Vec<String> = self.life.iter().map(|&l| near(i64::from(l))).collect();
        let counters = |values: &[u32]| {
            if values.iter().all(|&v| v == 0) {
                "-".to_string()
            } else {
                values
                    .iter()
                    .map(|&v| near_u(v))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        };
        format!(
            "{} {:?} [{}] life=[{}] gained=[{}] lost=[{}] n={}{}",
            if self.two_headed_giant { "2HG" } else { "duel" },
            self.engine,
            payoffs.join(", "),
            life.join(","),
            counters(&self.gained),
            counters(&self.lost),
            self.cycles,
            if self.takes > 1 {
                format!(" x{}", self.takes)
            } else {
                String::new()
            }
        )
    }
}

/// What a rules reader can read about one seat's life.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seat {
    life: i32,
    /// The total the seat's life changes adjust (CR 810.9), which a reader of the seat's life
    /// total reads (CR 810.9a).
    total: i64,
    gained: u32,
    lost: u32,
    lost_last_turn: u32,
    out: bool,
}

impl Seat {
    /// The seat's life field, the total it adjusts and its per-turn life counters: what a take
    /// that writes life changes.
    fn life_reading(&self) -> (i32, i64, u32, u32, u32) {
        (
            self.life,
            self.total,
            self.gained,
            self.lost,
            self.lost_last_turn,
        )
    }
}

/// The life state of the whole table and how the game stands.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Surface {
    seats: Vec<Seat>,
    standing: String,
}

fn standing(state: &GameState) -> String {
    match &state.waiting_for {
        WaitingFor::GameOver { winner } => format!("game over, winner {winner:?}"),
        WaitingFor::Priority { player } => format!("priority P{}", player.0),
        WaitingFor::LoopShortcut { .. } => "shortcut offer".into(),
        other => {
            let shown = format!("{other:?}");
            shown
                .split([' ', '{', '('])
                .next()
                .unwrap_or("")
                .to_string()
        }
    }
}

fn surface(state: &GameState) -> Surface {
    Surface {
        seats: state
            .players
            .iter()
            .map(|player| Seat {
                life: player.life,
                total: team_life_sum(state, player.id),
                gained: player.life_gained_this_turn,
                lost: player.life_lost_this_turn,
                lost_last_turn: player.life_lost_last_turn,
                out: player.is_eliminated,
            })
            .collect(),
        standing: standing(state),
    }
}

/// Every battlefield permanent as (controller, name, token, power, toughness, +1/+1 counters),
/// sorted: what a later reader of the board sees, whatever its object ids.
type Permanent = (u8, String, bool, Option<i32>, Option<i32>, u32);

fn board(state: &GameState) -> Vec<Permanent> {
    let mut permanents: Vec<Permanent> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|object| {
            (
                object.controller.0,
                object.name.clone(),
                object.is_token,
                object.power,
                object.toughness,
                object
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
            )
        })
        .collect();
    permanents.sort();
    permanents
}

fn is_over(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::GameOver { .. })
}

/// Scores 1 for a target choice that names one of `players`.
fn targets_one_of(players: &[PlayerId]) -> impl Fn(&GameAction) -> i32 + '_ {
    move |action| {
        let chosen = match action {
            GameAction::ChooseTarget {
                target: Some(target),
            } => std::slice::from_ref(target),
            GameAction::SelectTargets { targets } => targets.as_slice(),
            _ => &[],
        };
        i32::from(
            chosen.iter().any(
                |target| matches!(target, TargetRef::Player(player) if players.contains(player)),
            ),
        )
    }
}

fn at_empty_priority(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty()
}

/// A board driven to its shortcut offer.
struct Offer {
    state: GameState,
    engine: Engine,
    altar: ObjectId,
    fodder: ObjectId,
    tomb: ObjectId,
    korlis: ObjectId,
}

/// One copy of the offer's state, driven on.
struct Fork<'a> {
    runner: GameRunner,
    offer: &'a Offer,
}

impl Fork<'_> {
    /// Passes and answers until an empty-stack priority window, an offer or the game's end. Each
    /// answer names the creature where it can, chooses the engine's color, and targets an opponent
    /// of the loop's controller where a player is the target (Blood Artist: "target player loses 1
    /// life and you gain 1 life").
    fn settle(&mut self) {
        let color = match self.offer.engine {
            Engine::Black => ManaType::Black,
            Engine::White => ManaType::White,
        };
        let fodder = [self.offer.fodder];
        let opponents = players::opponents(self.runner.state(), P0);
        let score = |action: &GameAction| {
            2 * play_trace::names(&fodder)(action)
                + play_trace::chooses_color(color)(action)
                + targets_one_of(&opponents)(action)
        };
        play_trace::settle(&mut self.runner, &score);
    }

    fn over(&self) -> bool {
        is_over(self.runner.state())
    }

    fn decline_offers(&mut self) {
        while play_trace::is_offer(self.runner.state()) {
            self.runner
                .act(GameAction::DeclineShortcut)
                .expect("declining the shortcut is accepted");
            self.settle();
        }
    }

    fn return_fodder(&mut self) {
        play_trace::cast(
            &mut self.runner,
            self.offer.fodder,
            vec![],
            CastPaymentMode::Auto,
        );
        self.settle();
    }

    fn sacrifice_fodder(&mut self) {
        let index = play_trace::ability(self.runner.state(), self.offer.altar, true);
        play_trace::activate(&mut self.runner, self.offer.altar, index);
        self.settle();
    }

    /// Half a cycle by hand: the creature's return or its sacrifice, whichever the board is ready
    /// for. Leaves an offer standing.
    fn half_cycle(&mut self) {
        let in_graveyard = self.runner.state().objects[&self.offer.fodder].zone == Zone::Graveyard;
        if in_graveyard {
            self.return_fodder();
        } else {
            self.sacrifice_fodder();
        }
    }

    /// The reference: declines the offer and performs `cycles` cycles by hand, each a return and
    /// a sacrifice starting from whichever the board is ready for, stopping where the game ends.
    fn perform(&mut self, cycles: u32) {
        self.decline_offers();
        for _ in 0..2 * cycles {
            if self.over() {
                return;
            }
            let in_graveyard =
                self.runner.state().objects[&self.offer.fodder].zone == Zone::Graveyard;
            if in_graveyard {
                self.return_fodder();
            } else {
                self.sacrifice_fodder();
            }
            self.decline_offers();
        }
    }

    /// Declares `Fixed(cycles)` at the standing offer and every responder accepts; returns how
    /// the take was carried out, read off its events and off the life it wrote. A count above the
    /// offer's capacity is checked to emit nothing and change nothing.
    fn take(&mut self, cycles: u32) -> Route {
        let before = self.runner.state();
        let WaitingFor::LoopShortcut { schema, .. } = &before.waiting_for else {
            panic!("a take is declared at an offer");
        };
        let capacity = schema.deliverable_capacity;
        let (seats, permanents) = (surface(before).seats, board(before));
        let mut events = self
            .runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(cycles),
                template: None,
            })
            .expect("the shortcut declaration is accepted")
            .events;
        while matches!(
            self.runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ) {
            events.extend(
                self.runner
                    .act(GameAction::RespondToShortcut {
                        response: ShortcutResponse::Accept,
                    })
                    .expect("accepting the shortcut is accepted")
                    .events,
            );
        }
        let after = self.runner.state();
        if cycles > capacity {
            assert!(events.is_empty(), "a refused declaration emits no event");
            assert_eq!(
                (surface(after).seats, board(after)),
                (seats, permanents),
                "a refused declaration changes nothing"
            );
            return Route::Refused;
        }
        let wrote_life = events
            .iter()
            .any(|event| matches!(event, GameEvent::LifeChanged { .. }))
            || surface(after)
                .seats
                .iter()
                .zip(&seats)
                .any(|(after, before)| after.life_reading() != before.life_reading());
        match (markers(&events).is_empty(), wrote_life) {
            (false, _) => Route::Performed,
            (true, true) => Route::Batched,
            (true, false) => Route::BatchedWithoutLife,
        }
    }

    /// P0 taps Ancient Tomb for mana and takes its 2 damage (CR 120.3a).
    fn tap_the_tomb(&mut self) {
        let index = play_trace::ability(self.runner.state(), self.offer.tomb, true);
        play_trace::activate(&mut self.runner, self.offer.tomb, index);
        self.settle();
    }

    /// P0 sacrifices Children of Korlis and gains the life it lost this turn.
    fn sacrifice_korlis(&mut self) {
        let index = play_trace::ability(self.runner.state(), self.offer.korlis, false);
        play_trace::activate(&mut self.runner, self.offer.korlis, index);
        self.settle();
    }

    /// Passes the turn forward until the end step holds priority with an empty stack, declaring
    /// no attackers, ordering triggers as listed and collapsing a stashed loop at its maximum.
    fn reach_the_end_step(&mut self) {
        for _ in 0..200 {
            let state = self.runner.state();
            if is_over(state) || (state.phase == Phase::End && at_empty_priority(state)) {
                return;
            }
            match &state.waiting_for {
                WaitingFor::Priority { .. } => {
                    self.runner
                        .act(GameAction::PassPriority)
                        .expect("passing priority is accepted");
                }
                WaitingFor::DeclareAttackers { .. } => {
                    self.runner
                        .declare_attackers(&[])
                        .expect("declaring no attackers is accepted");
                }
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let order = (0..triggers.len()).collect();
                    self.runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("the identity order is accepted");
                }
                WaitingFor::PayAmountChoice {
                    resource: PayableResource::LoopCollapse { .. },
                    max,
                    ..
                } => {
                    let amount = *max;
                    self.runner
                        .act(GameAction::SubmitPayAmount { amount })
                        .expect("the collapse amount is accepted");
                }
                other => panic!("unexpected prompt on the way to the end step: {other:?}"),
            }
        }
        panic!("the end step was not reached within 200 beats");
    }
}

/// Builds `spec`'s board and performs cycles by hand, at most three, until the shortcut is offered.
/// `None` where none is offered within three cycles or the game ends first.
fn offer(db: &'static CardDatabase, spec: &Spec) -> Option<Offer> {
    let mut scenario = if spec.two_headed_giant {
        GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 7)
    } else {
        GameScenario::new_n_player(2, 7)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for (seat, &life) in spec.life.iter().enumerate() {
        scenario.with_life(PlayerId(seat as u8), life);
    }
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
    let fodder = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    if spec.engine == Engine::White {
        scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db);
    }
    for &(seat, card) in &spec.payoffs {
        match card {
            Card::Printed(name) => {
                scenario.add_real_card(seat, name, Zone::Battlefield, db);
            }
            Card::Creature(name, power, toughness, text) => {
                scenario.add_creature_from_oracle(seat, name, power, toughness, text);
            }
        }
    }
    let tomb = scenario.add_real_card(P0, "Ancient Tomb", Zone::Battlefield, db);
    let korlis = scenario
        .add_creature_from_oracle(P0, "Children of Korlis", 1, 1, CHILDREN_OF_KORLIS)
        .id();
    for seat in 0..spec.life.len() {
        for _ in 0..10 {
            scenario.add_real_card(PlayerId(seat as u8), "Swamp", Zone::Library, db);
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    for seat in 0..spec.life.len() {
        runner.state_mut().players[seat].life_gained_this_turn = spec.gained[seat];
        runner.state_mut().players[seat].life_lost_this_turn = spec.lost[seat];
    }
    let mut offer = Offer {
        state: runner.state().clone(),
        engine: spec.engine,
        altar,
        fodder,
        tomb,
        korlis,
    };
    // Up to three cycles: a loop whose first cycle changes the board is offered a cycle later.
    // The offer may stand between the creature's return and its sacrifice; the reference then
    // continues from there (`Fork::perform` reads where the creature is).
    let state = {
        let mut fork = Fork {
            runner,
            offer: &offer,
        };
        for half in 0..6 {
            if fork.over() || play_trace::is_offer(fork.runner.state()) {
                break;
            }
            if half % 2 == 0 {
                fork.return_fodder();
            } else {
                fork.sacrifice_fodder();
            }
        }
        fork.runner.state().clone()
    };
    if !play_trace::is_offer(&state) {
        return None;
    }
    offer.state = state;
    Some(offer)
}

/// One stage's readings of both copies.
#[derive(Debug)]
struct Stage {
    name: &'static str,
    taken: (Surface, Vec<Permanent>),
    performed: (Surface, Vec<Permanent>),
}

/// How a take was carried out, read off its record and the life it wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Route {
    /// The record holds repetition markers: the engine performed the cycles.
    Performed,
    /// No marker, and the take wrote life (a `LifeChanged` event, or a seat's life field, the
    /// total it adjusts or a per-turn life counter moved): the take applied its life growth
    /// without performing a cycle.
    Batched,
    /// No marker and no life written: the take applied growth on another axis only (the mana a
    /// White engine nets on a board where no life changes, under Sulfuric Vortex or Platinum
    /// Emperion). Compared like any take, but it is not the batched route this test is here to
    /// compare, so it satisfies no family's requirement.
    BatchedWithoutLife,
    /// CR 732.2a: the count is above the capacity the offer published (some player would cross a
    /// loss threshold inside the proposal), so the declaration is handed back to manual play: no
    /// player is asked to respond and nothing is performed. Not a take, so nothing is compared.
    Refused,
}

#[derive(Debug)]
struct RowResult {
    /// Each take's route, in the order taken.
    routes: Vec<Route>,
    /// The number of the later take whose offer did not stand within the cycles by hand allowed
    /// before it, where the row's takes stopped.
    unreached: Option<u32>,
    offered: Vec<String>,
    stages: Vec<Stage>,
}

/// Runs one row: the take and the reference from one copied state, read at every stage.
fn run_row(db: &'static CardDatabase, spec: &Spec) -> Option<RowResult> {
    let offer = offer(db, spec)?;
    let offered = match &offer.state.waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => certificate
            .unbounded
            .iter()
            .map(|axis| format!("{axis:?}"))
            .collect(),
        _ => Vec::new(),
    };
    let mut taken = Fork {
        runner: GameRunner::from_state(offer.state.clone()),
        offer: &offer,
    };
    let mut performed = Fork {
        runner: GameRunner::from_state(offer.state.clone()),
        offer: &offer,
    };
    let route = taken.take(spec.cycles);
    if route == Route::Refused {
        return Some(RowResult {
            routes: vec![route],
            unreached: None,
            offered,
            stages: Vec::new(),
        });
    }
    let mut routes = vec![route];
    performed.perform(spec.cycles);

    // Each later take: by hand to the next offer (at most two cycles), then the same count again.
    // The reference performs the same half cycles, then the count. A later offer that does not
    // stand within the two cycles stops the row's takes, and the row fails. A refused later take
    // leaves both copies where the half cycles left them.
    let mut unreached = None;
    for take in 2..=spec.takes {
        let mut by_hand = 0;
        while by_hand < 4
            && at_empty_priority(taken.runner.state())
            && !play_trace::is_offer(taken.runner.state())
        {
            taken.half_cycle();
            by_hand += 1;
        }
        for _ in 0..by_hand {
            if at_empty_priority(performed.runner.state()) {
                performed.half_cycle();
                performed.decline_offers();
            }
        }
        if !play_trace::is_offer(taken.runner.state()) {
            unreached = Some(take);
            break;
        }
        let route = taken.take(spec.cycles);
        routes.push(route);
        if route == Route::Refused {
            break;
        }
        performed.perform(spec.cycles);
    }

    let mut stages = Vec::new();
    let read = |name: &'static str, taken: &Fork, performed: &Fork, stages: &mut Vec<Stage>| {
        stages.push(Stage {
            name,
            taken: (surface(taken.runner.state()), board(taken.runner.state())),
            performed: (
                surface(performed.runner.state()),
                board(performed.runner.state()),
            ),
        });
    };
    read("take", &taken, &performed, &mut stages);
    let both_ready = |taken: &Fork, performed: &Fork| {
        at_empty_priority(taken.runner.state()) && at_empty_priority(performed.runner.state())
    };
    if both_ready(&taken, &performed) {
        taken.tap_the_tomb();
        performed.tap_the_tomb();
        read("Ancient Tomb", &taken, &performed, &mut stages);
    }
    if both_ready(&taken, &performed) {
        taken.sacrifice_korlis();
        performed.sacrifice_korlis();
        read("Children of Korlis", &taken, &performed, &mut stages);
    }
    if both_ready(&taken, &performed) {
        taken.reach_the_end_step();
        performed.reach_the_end_step();
        read("end step", &taken, &performed, &mut stages);
    }
    Some(RowResult {
        routes,
        unreached,
        offered,
        stages,
    })
}

fn describe(surface: &Surface) -> String {
    let seats: Vec<String> = surface
        .seats
        .iter()
        .map(|seat| {
            format!(
                "({} total {} +{} -{}{})",
                near(i64::from(seat.life)),
                near(seat.total),
                near_u(seat.gained),
                near_u(seat.lost),
                if seat.out { " OUT" } else { "" }
            )
        })
        .collect();
    format!("{} | {}", seats.join(" "), surface.standing)
}

/// Runs every row of `specs` whose index is `shard` modulo `shards`, and fails with the full list
/// of rows whose take and reference differ, and of rows with a later take whose offer did not
/// stand. A row whose board is offered no shortcut within three cycles compares nothing, so the
/// shard also fails when more than `no_offer_ceiling` of its rows are offered none.
///
/// `expected` names, for each take of the shard's rows in turn (the first, then a second), the
/// routes that take has to reach, and the shard fails if its rows miss one. A take that performs
/// its cycles is compared with the same cycles performed by hand, which checks the engine's
/// replay; the batched route is the one that writes life without performing a cycle. So a family
/// that names `Route::Batched` fails when a change stops batching its rows, where it would
/// otherwise pass on performed takes alone.
fn run_family(
    family: &str,
    specs: Vec<Spec>,
    shard: usize,
    shards: usize,
    expected: &[&[Route]],
    no_offer_ceiling: usize,
) {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut rows = 0;
    // Per take, in turn: how many of the shard's takes went each route.
    let mut by_take: Vec<BTreeMap<Route, usize>> = Vec::new();
    let mut no_offer = Vec::new();
    let mut failures = Vec::new();
    for spec in specs.iter().skip(shard).step_by(shards) {
        rows += 1;
        let label = spec.label();
        let row = match catch_unwind(AssertUnwindSafe(|| run_row(db, spec))) {
            Ok(Some(row)) => row,
            Ok(None) => {
                no_offer.push(label);
                continue;
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                failures.push(format!("HARNESS {label}\n    {message}"));
                continue;
            }
        };
        for (take, &route) in row.routes.iter().enumerate() {
            if by_take.len() == take {
                by_take.push(BTreeMap::new());
            }
            *by_take[take].entry(route).or_default() += 1;
        }
        if let Some(take) = row.unreached {
            failures.push(format!(
                "REACH {label}\n    routes {:?}: take {take} of {} was offered no shortcut within two cycles by hand",
                row.routes, spec.takes
            ));
        }
        if let Some(stage) = row
            .stages
            .iter()
            .find(|stage| stage.taken.0 != stage.performed.0)
        {
            failures.push(format!(
                "LIFE  {label}\n    routes {:?}, offer {:?}, first differs after: {}\n    taken:     {}\n    performed: {}",
                row.routes,
                row.offered,
                stage.name,
                describe(&stage.taken.0),
                describe(&stage.performed.0)
            ));
        } else if let Some(stage) = row
            .stages
            .iter()
            .find(|stage| stage.taken.1 != stage.performed.1)
        {
            let only = |a: &[Permanent], b: &[Permanent]| -> Vec<Permanent> {
                a.iter().filter(|p| !b.contains(p)).cloned().collect()
            };
            failures.push(format!(
                "BOARD {label}\n    routes {:?}, first differs after: {}\n    only taken:     {:?}\n    only performed: {:?}",
                row.routes,
                stage.name,
                only(&stage.taken.1, &stage.performed.1),
                only(&stage.performed.1, &stage.taken.1)
            ));
        }
    }
    let routes: Vec<String> = by_take
        .iter()
        .enumerate()
        .map(|(take, routes)| match take {
            0 => format!("routes {routes:?}"),
            _ => format!("take {} routes {routes:?}", take + 1),
        })
        .collect();
    let summary = format!(
        "{family} shard {shard}/{shards}: {rows} rows, {}, no offer {}",
        routes.join(", "),
        no_offer.len()
    );
    eprintln!("{summary}");
    assert!(
        failures.is_empty(),
        "{summary}\n{}\nNOOFFER {}",
        failures.join("\n"),
        no_offer.join("\nNOOFFER ")
    );
    assert!(
        no_offer.len() <= no_offer_ceiling,
        "{summary}: more rows than this shard's {no_offer_ceiling} were offered no shortcut\nNOOFFER {}",
        no_offer.join("\nNOOFFER ")
    );
    for (take, required) in expected.iter().enumerate() {
        for route in *required {
            assert!(
                by_take
                    .get(take)
                    .is_some_and(|routes| routes.contains_key(route)),
                "{summary}: no take {} took the {route:?} route, which this family is here to compare",
                take + 1
            );
        }
    }
}

fn duel(payoffs: &[(PlayerId, Card)]) -> Spec {
    Spec::new(false, payoffs)
}

fn giants(payoffs: &[(PlayerId, Card)]) -> Spec {
    Spec::new(true, payoffs)
}

/// Each counter preset as (seat, gained, lost) triples applied over a spec.
fn with_counters(spec: &Spec, presets: &[&[(usize, u32, u32)]]) -> Vec<Spec> {
    presets
        .iter()
        .map(|preset| {
            let mut spec = spec.clone();
            for &(seat, gained, lost) in *preset {
                spec.gained[seat] = gained;
                spec.lost[seat] = lost;
            }
            spec
        })
        .collect()
}

/// A cycle that only gains life, in a duel. Each payoff stores its gain differently: two a cycle
/// on the loop's controller, on the opponent's field, on both fields, or doubled by a replacement
/// effect (Rhox Faithmender: "If you would gain life, you gain twice that much life instead.").
fn gain_specs() -> Vec<Spec> {
    let printed = Card::Printed;
    let payoff_sets: Vec<Vec<(PlayerId, Card)>> = vec![
        vec![(P0, WARDEN), (P0, WARDEN)],
        vec![(P1, WARDEN)],
        vec![(P0, WARDEN), (P1, WARDEN)],
        vec![(P0, WARDEN), (P0, printed("Rhox Faithmender"))],
    ];
    // No counter set; P0's gained counter three short of its pin, so a take on a board where P0
    // gains crosses it (with only P1's Soul Warden it stays put); P0's lost counter at its pin.
    let counters: &[&[(usize, u32, u32)]] = &[&[], &[(0, PIN - 3, 0)], &[(0, 0, PIN)]];
    let mut specs = Vec::new();
    for payoffs in &payoff_sets {
        for engine in [Engine::Black, Engine::White] {
            // The offer stands after one cycle performed by hand (+2), then the take's three
            // cycles gain 6: from MAX-7 the take passes the cap in its last cycle, from MAX-5 in
            // its second, from MAX-1 the cap is reached before the offer, and at MAX it holds the
            // whole take. Takes that stay short of the cap near it, or land on it exactly, are
            // the rows with one Soul Warden per seat and the forty-cycle rows below.
            for p0 in [20, MAX - 7, MAX - 5, MAX - 1, MAX] {
                for p1 in [20, MAX] {
                    let mut spec = duel(payoffs);
                    spec.engine = engine;
                    spec.life = vec![p0, p1];
                    specs.extend(with_counters(&spec, counters));
                }
            }
        }
    }
    // Forty cycles of two Soul Wardens' 2: short of the cap, reaching it exactly, and one past.
    for engine in [Engine::Black, Engine::White] {
        for p0 in [MAX - 200, MAX - 82, MAX - 81] {
            let mut spec = duel(&[(P0, WARDEN), (P0, WARDEN)]);
            spec.engine = engine;
            spec.life = vec![p0, 20];
            spec.cycles = 40;
            specs.extend(with_counters(&spec, counters));
        }
    }
    specs
}

/// A cycle that loses life, alone or beside a gain, in a duel. A seat that loses life on net keeps
/// an ordinary total here, except the seat Platinum Angel keeps in the game, which is drained down
/// to `i32::MIN`; the drains with a total near `i32::MAX` are [`drain_near_limit_specs`].
fn loss_specs() -> Vec<Spec> {
    let printed = Card::Printed;
    // (payoffs, P0's life levels, P1's life levels)
    type Set = (Vec<(PlayerId, Card)>, &'static [i32], &'static [i32]);
    let sets: Vec<Set> = vec![
        // P0 nets a gain, so its total may stand at the cap.
        (
            vec![(P0, CARNIVAL), (P0, WARDEN), (P0, WARDEN)],
            &[20, MAX - 2, MAX],
            &[20],
        ),
        // P0 nets nothing: a gain and a loss of 1 each cycle.
        (vec![(P0, WARDEN), (P0, CARNIVAL)], &[20], &[20]),
        // P0 loses on net. At 3 life the offer's capacity is 2, so a count of 3 is refused.
        (vec![(P0, CARNIVAL)], &[20, 3], &[20]),
        // A drain with no target, and a targeted one, against an opponent at 20, and at 4: the
        // offer stands at 3 and the take's last cycle ends the game.
        (vec![(P0, printed("Zulaport Cutthroat"))], &[20], &[20, 4]),
        (
            vec![(P0, WARDEN), (P0, printed("Vito, Thorn of the Dusk Rose"))],
            &[20],
            &[20, 4],
        ),
        // A drain against a seat Platinum Angel keeps in the game, down to the bottom limit (at -5
        // the count is refused).
        (
            vec![
                (P0, printed("Zulaport Cutthroat")),
                (P1, printed("Platinum Angel")),
            ],
            &[20],
            &[20, -5, MIN + 1, MIN],
        ),
    ];
    // No counter set; P0's lost counter at its pin; P1's lost counter three short of its pin.
    let counters: &[&[(usize, u32, u32)]] = &[&[], &[(0, 0, PIN)], &[(1, 0, PIN - 3)]];
    let mut specs = Vec::new();
    for (payoffs, p0_lives, p1_lives) in &sets {
        for &p0 in *p0_lives {
            for &p1 in *p1_lives {
                let mut spec = duel(payoffs);
                spec.life = vec![p0, p1];
                specs.extend(with_counters(&spec, counters));
            }
        }
    }
    specs
}

/// Boards whose cycle loses life (a drain, a loss alone, or a gain and a loss of 1 each) while a
/// total stands near `i32::MAX`: the offer's life-crossing read runs over the whole total
/// (`analysis::resource`'s `cascade_horizon`).
fn drain_near_limit_specs() -> Vec<Spec> {
    let printed = Card::Printed;
    let mut specs = Vec::new();
    let mut add = |payoffs: &[(PlayerId, Card)], life: Vec<i32>, two_headed_giant: bool| {
        let mut spec = Spec::new(two_headed_giant, payoffs);
        spec.life = life;
        specs.push(spec);
    };
    add(&[(P0, WARDEN), (P0, CARNIVAL)], vec![MAX - 2, 20], false);
    add(&[(P0, WARDEN), (P0, CARNIVAL)], vec![MAX, 20], false);
    add(&[(P0, CARNIVAL)], vec![MAX - 2, 20], false);
    add(
        &[(P0, printed("Zulaport Cutthroat"))],
        vec![MAX - 4, 20],
        false,
    );
    add(&[(P0, printed("Zulaport Cutthroat"))], vec![MAX, 20], false);
    add(
        &[(P0, printed("Zulaport Cutthroat"))],
        vec![20, 20, MAX - 3, 1],
        true,
    );
    add(
        &[(P0, printed("Zulaport Cutthroat"))],
        vec![MAX - 3, -20, 20, 20],
        true,
    );
    // Targeted drains: the charged slot's pin is read at every repetition the walk steps through.
    add(
        &[(P0, WARDEN), (P0, printed("Vito, Thorn of the Dusk Rose"))],
        vec![MAX - 4, 20],
        false,
    );
    add(
        &[(P0, WARDEN), (P0, printed("Sanguine Bond"))],
        vec![MAX - 4, 20],
        false,
    );
    // Blood Artist's "target player" is the opponent (`Fork::settle`).
    add(&[(P0, printed("Blood Artist"))], vec![MAX - 4, 20], false);
    add(
        &[(P0, WARDEN), (P0, printed("Vito, Thorn of the Dusk Rose"))],
        vec![MAX - 4, 20, 20, 20],
        true,
    );
    specs
}

/// A cycle whose life something on the board watches, replaces or forbids, in a duel: a counter
/// that grows with each gain (Ajani's Pridemate), an end-step reader of the gained counter
/// (Angelic Accord), a body that reads its controller's total (Serra Ascendant, from 27 life, and
/// Serra Avatar: "Serra Avatar's power and toughness are each equal to your life total."), a gain
/// that grows with the spells cast (Aetherflux Reservoir), a gain forbidden (Sulfuric Vortex) and
/// a total that cannot change (Platinum Emperion).
///
/// At 27 life only Serra Avatar's takes write life without performing a cycle: Ajani's Pridemate
/// and Angelic Accord read the gain through a trigger, Serra Ascendant through a condition,
/// Aetherflux Reservoir's gain comes from a cast trigger, and under Sulfuric Vortex or Platinum
/// Emperion no life changes. Serra Avatar reads its controller's total with no condition, so its
/// life lands at the take and its body is compared after it.
fn observed_specs() -> Vec<Spec> {
    let printed = Card::Printed;
    let payoff_sets: Vec<Vec<(PlayerId, Card)>> = vec![
        vec![(P0, WARDEN), (P0, printed("Ajani's Pridemate"))],
        vec![(P0, WARDEN), (P0, printed("Angelic Accord"))],
        vec![(P0, WARDEN), (P0, SERRA)],
        vec![(P0, WARDEN), (P0, printed("Serra Avatar"))],
        vec![(P0, printed("Aetherflux Reservoir"))],
        vec![(P0, WARDEN), (P0, printed("Sulfuric Vortex"))],
        vec![(P0, WARDEN), (P0, printed("Platinum Emperion"))],
    ];
    let counters: &[&[(usize, u32, u32)]] = &[&[], &[(0, PIN - 3, 0)]];
    let mut specs = Vec::new();
    for payoffs in &payoff_sets {
        for engine in [Engine::Black, Engine::White] {
            for p0 in [27, MAX - 3, MAX] {
                let mut spec = duel(payoffs);
                spec.engine = engine;
                spec.life = vec![p0, 20];
                specs.extend(with_counters(&spec, counters));
            }
        }
    }
    specs
}

/// Two-Headed Giant: P0 and P1 share a total, as do P2 and P3 (CR 810.4).
fn giant_specs() -> Vec<Spec> {
    let printed = Card::Printed;
    let payoff_sets: Vec<Vec<(PlayerId, Card)>> = vec![
        vec![(P0, WARDEN), (P0, WARDEN)],
        vec![(P2, WARDEN)],
        vec![(P0, WARDEN), (P2, WARDEN)],
        vec![(P0, CARNIVAL), (P0, WARDEN), (P0, WARDEN)],
        vec![(P0, printed("Zulaport Cutthroat"))],
    ];
    // Team A's fields: ordinary; one field above the shared total; the shared total exactly at
    // the cap; a negative field.
    let team_a: &[(i32, i32)] = &[(20, 20), (MAX - 3, -20), (MAX - 5, 5), (-20, 60)];
    let team_b: &[(i32, i32)] = &[(20, 20), (MAX - 3, 1)];
    let counters: &[&[(usize, u32, u32)]] = &[&[], &[(0, PIN, 0)], &[(0, PIN - 3, 0)]];
    // The drain keeps to ordinary totals (see [`loss_specs`]). Team B at 8 and 1 is offered at a
    // shared total of 7, and the take leaves it at 1 with one field below 0. A take whose last
    // cycle would end the game is left out: from fields 6 and 1, 7 and 1 or 5 and 2 the take stops
    // one cycle short of the lethal cycle that performing reaches, a difference upstream `main`
    // already has at ordinary totals, apart from this change.
    let drained_a: &[(i32, i32)] = &[(20, 20), (3, 2)];
    let drained_b: &[(i32, i32)] = &[(20, 20), (8, 1)];
    let mut specs = Vec::new();
    for payoffs in &payoff_sets {
        let drain = payoffs.contains(&(P0, printed("Zulaport Cutthroat")));
        let (team_a, team_b) = if drain {
            (drained_a, drained_b)
        } else {
            (team_a, team_b)
        };
        for &(p0, p1) in team_a {
            for &(p2, p3) in team_b {
                let mut spec = giants(payoffs);
                spec.life = vec![p0, p1, p2, p3];
                specs.extend(with_counters(&spec, counters));
            }
        }
    }
    specs
}

/// Two shortcuts taken in one step, a cycle that only gains life. With both Soul Wardens under P0,
/// the cycle by hand that offers the first take, two takes of three cycles and the cycle by hand
/// between them gain P0's total 16, so from MAX-17 the second take stays short of the cap, from
/// MAX-16 it reaches it exactly and from MAX-15 it passes it. The Two-Headed Giant rows with one
/// Soul Warden per team gain team A's total 8, so they stay short of the cap.
fn twice_specs() -> Vec<Spec> {
    let counters: &[&[(usize, u32, u32)]] = &[&[], &[(0, PIN, 0)], &[(0, PIN - 9, 0)]];
    let mut specs = Vec::new();
    for engine in [Engine::Black, Engine::White] {
        for p0 in [20, MAX - 17, MAX - 16, MAX - 15, MAX] {
            let mut spec = duel(&[(P0, WARDEN), (P0, WARDEN)]);
            spec.engine = engine;
            spec.life = vec![p0, 20];
            spec.takes = 2;
            specs.extend(with_counters(&spec, counters));
        }
    }
    for payoffs in [
        vec![(P0, WARDEN), (P0, WARDEN)],
        vec![(P0, WARDEN), (P2, WARDEN)],
    ] {
        for (p0, p1) in [(20, 20), (MAX - 17, 0), (MAX - 16, 0)] {
            let mut spec = giants(&payoffs);
            spec.life = vec![p0, p1, 20, 20];
            spec.takes = 2;
            specs.extend(with_counters(&spec, counters));
        }
    }
    specs
}

/// A family whose rows sit on both sides of a limit: some takes batch and some perform.
const BATCHED_AND_PERFORMED: &[Route] = &[Route::Batched, Route::Performed];

/// One test per shard of a family, each written `name = shard no_offer ceiling`: the shard's
/// index and the most of its rows that may be offered no shortcut. `$expected` lists, per take
/// in turn, the routes that take must reach in every shard (`run_family`).
macro_rules! shards {
    (
        $family:ident,
        $specs:ident,
        $expected:expr,
        [$($name:ident = $shard:literal no_offer $ceiling:literal),* $(,)?],
        $shards:literal
    ) => {
        $(
            #[test]
            fn $name() {
                run_family(
                    stringify!($family),
                    $specs(),
                    $shard,
                    $shards,
                    &$expected,
                    $ceiling,
                );
            }
        )*
    };
}

shards!(
    gain,
    gain_specs,
    [BATCHED_AND_PERFORMED],
    [
        gain_0 = 0 no_offer 5,
        gain_1 = 1 no_offer 6,
        gain_2 = 2 no_offer 4,
        gain_3 = 3 no_offer 6,
        gain_4 = 4 no_offer 5,
        gain_5 = 5 no_offer 7,
        gain_6 = 6 no_offer 5,
        gain_7 = 7 no_offer 7,
    ],
    8
);
// A cycle that loses life is never batched. Two boards declare a count above capacity: Carnival of
// Souls alone at 3 life, and Zulaport Cutthroat against Platinum Angel's controller at -5.
shards!(
    loss,
    loss_specs,
    [&[Route::Performed, Route::Refused]],
    [loss_0 = 0 no_offer 0],
    1
);
shards!(
    drain_near_limit,
    drain_near_limit_specs,
    [&[Route::Performed]],
    [
        drain_near_limit_0 = 0 no_offer 0,
        drain_near_limit_1 = 1 no_offer 0,
    ],
    2
);
shards!(
    observed,
    observed_specs,
    [BATCHED_AND_PERFORMED],
    [observed_0 = 0 no_offer 11, observed_1 = 1 no_offer 11],
    2
);
shards!(
    giant,
    giant_specs,
    [BATCHED_AND_PERFORMED],
    [
        giant_0 = 0 no_offer 7,
        giant_1 = 1 no_offer 7,
        giant_2 = 2 no_offer 7,
    ],
    3
);
// A shard's first takes and its second takes must each reach both routes: a second take that
// lands on the cap exactly is batched (MAX-16), and one that passes it is performed (MAX-15).
shards!(
    twice,
    twice_specs,
    [BATCHED_AND_PERFORMED, BATCHED_AND_PERFORMED],
    [twice_0 = 0 no_offer 2, twice_1 = 1 no_offer 1],
    2
);
