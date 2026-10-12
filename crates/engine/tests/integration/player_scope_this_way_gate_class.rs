//! The "each player <verb>s … <noun> <verb>ed this way" class (#9225), beyond
//! Locke: a "this way" gate that follows a `player_scope` instruction is ONE
//! look-back over the whole multi-player action unless the gated clause names
//! the iterated player.
//!
//! CR anchors:
//!   - CR 608.2f: an action taken on multiple players is one action, processed
//!     per player only when it can't be processed simultaneously.
//!   - CR 608.2c: the following instruction reads that action's result, and
//!     "that many" names the population of the gate it follows (the rules of
//!     English: its nearest antecedent).
//!
//! Every fixture has four players, because a per-player defect in this class
//! can coincide with the correct answer in a two-player game.

use std::collections::HashMap;

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::FrameKind;
use engine::types::zones::Zone;

const PLAYER_COUNT: u8 = 4;
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const ALL_PLAYERS: [PlayerId; 4] = [P0, P1, P2, P3];

/// Verbatim Oracle text (Scryfall `cards/named?exact=Augusta, Order Returned`).
const AUGUSTA: &str = "Flying, vigilance\n\
     Whenever Augusta attacks, each player exiles a card from their graveyard. \
     When one or more nonland cards are exiled this way, put that many +1/+1 \
     counters on target attacking creature.";

/// Four players, each with exactly one card in their graveyard: a nonland card
/// for every player in `nonland_exiled_by`, a land for the rest. Augusta (P0)
/// attacks alone, so she is the only legal "target attacking creature".
/// Returns the runner after the trigger has resolved, Augusta, and the staged
/// graveyard cards in seat order.
fn augusta_attacks(nonland_exiled_by: &[PlayerId]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9225);
    scenario.at_phase(Phase::PreCombatMain);
    let augusta = scenario
        .add_creature_from_oracle(P0, "Augusta, Order Returned", 2, 2, AUGUSTA)
        .id();
    let staged: Vec<ObjectId> = ALL_PLAYERS
        .iter()
        .map(|&player| {
            if nonland_exiled_by.contains(&player) {
                scenario
                    .add_creature_to_graveyard(player, "Exiled Creature Card", 1, 1)
                    .id()
            } else {
                scenario
                    .add_land_to_graveyard(player, "Exiled Land Card")
                    .id()
            }
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(augusta, AttackTarget::Player(P1))])
        .expect("Augusta must be able to attack");
    runner.advance_until_stack_empty();
    (runner, augusta, staged)
}

fn plus_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_every_staged_card_was_exiled(runner: &GameRunner, staged: &[ObjectId]) {
    for (seat, id) in staged.iter().enumerate() {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "reach guard: player {seat} must have exiled their only graveyard card"
        );
    }
}

/// CR 608.2c + CR 608.2f: every player exiles a nonland card, so "that many"
/// is four, placed once on Augusta.
#[test]
fn augusta_counts_every_players_nonland_exile() {
    let (runner, augusta, staged) = augusta_attacks(&ALL_PLAYERS);
    assert_every_staged_card_was_exiled(&runner, &staged);

    assert_eq!(
        plus_counters(&runner, augusta),
        4,
        "four nonland cards were exiled this way — Augusta gets four counters"
    );
}

/// CR 608.2c: "that many" is the NONLAND cards exiled this way, not every card
/// the instruction exiled. Two nonland cards and two lands → two counters. A
/// count of the whole clause result would give four.
#[test]
fn augusta_counts_only_the_nonland_cards_exiled() {
    let (runner, augusta, staged) = augusta_attacks(&[P0, P1]);
    assert_every_staged_card_was_exiled(&runner, &staged);

    assert_eq!(
        plus_counters(&runner, augusta),
        2,
        "only the two nonland cards count toward \"that many\""
    );
}

/// CR 608.2c: no nonland card was exiled, so the gate is false and no counter
/// is placed — the gate discriminates rather than always firing.
#[test]
fn augusta_places_no_counter_when_only_lands_were_exiled() {
    let (runner, augusta, staged) = augusta_attacks(&[]);
    assert_every_staged_card_was_exiled(&runner, &staged);

    assert_eq!(plus_counters(&runner, augusta), 0);
}

// ── Prompted seats: the clause owner frame ────────────────────────────────────
//
// Every fixture below gives a seat a nonland AND a land card, so each such seat
// is prompted (`EffectZoneChoice`) instead of exiling automatically. A prompted
// clause parks a `PlayerScopeClause` owner that publishes the whole clause once,
// immediately before the "that many" tail (CR 608.2f + CR 101.4 + CR 608.2c).

/// One seat's staged graveyard.
#[derive(Clone, Copy)]
struct Graveyard {
    nonland: bool,
    land: bool,
}

const BOTH: Graveyard = Graveyard {
    nonland: true,
    land: true,
};
const NONLAND_ONLY: Graveyard = Graveyard {
    nonland: true,
    land: false,
};
const EMPTY: Graveyard = Graveyard {
    nonland: false,
    land: false,
};

/// Which card a prompted seat exiles.
#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Nonland,
    Land,
}

struct PromptedAugusta {
    runner: GameRunner,
    augusta: ObjectId,
    nonland: Vec<Option<ObjectId>>,
    land: Vec<Option<ObjectId>>,
    hand: Vec<ObjectId>,
}

/// Four players with the staged graveyards (and hand cards, for leaver rows);
/// Augusta (P0) attacks alone. The trigger is on the stack, unresolved.
fn augusta_with_choices(
    graveyards: [Graveyard; 4],
    hand_cards: &[(PlayerId, usize)],
) -> PromptedAugusta {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9225);
    scenario.at_phase(Phase::PreCombatMain);
    let augusta = scenario
        .add_creature_from_oracle(P0, "Augusta, Order Returned", 2, 2, AUGUSTA)
        .id();
    let mut nonland = Vec::new();
    let mut land = Vec::new();
    for player in ALL_PLAYERS {
        let staged = graveyards[player.0 as usize];
        nonland.push(staged.nonland.then(|| {
            scenario
                .add_creature_to_graveyard(player, "Nonland Card", 1, 1)
                .id()
        }));
        land.push(
            staged
                .land
                .then(|| scenario.add_land_to_graveyard(player, "Land Card").id()),
        );
    }
    let mut hand = Vec::new();
    for &(player, count) in hand_cards {
        for index in 0..count {
            hand.push(
                scenario
                    .add_creature_to_hand(player, &format!("Hand Bear {index}"), 2, 2)
                    .id(),
            );
        }
    }
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(augusta, AttackTarget::Player(P1))])
        .expect("Augusta must be able to attack");
    PromptedAugusta {
        runner,
        augusta,
        nonland,
        land,
        hand,
    }
}

/// A departure planned at a prompt: `who` leaves when `when`'s prompt stands.
#[derive(Clone, Copy, Default)]
struct Plan {
    /// CR 104.3a: `who` concedes.
    concede: Option<(PlayerId, PlayerId)>,
    /// CR 704.5a: `who`'s life drops to 0 before `when` answers.
    kill: Option<(PlayerId, PlayerId)>,
}

#[derive(Default)]
struct Drive {
    /// `(seat, candidate count)` of every exile prompt, in order.
    prompts: Vec<(PlayerId, usize)>,
    /// The resolution-stack frame kinds standing at each prompt.
    shapes: Vec<Vec<FrameKind>>,
    /// Whether each answer's events placed a +1/+1 counter.
    counters_per_answer: Vec<bool>,
    /// Whether the planned concession was accepted.
    conceded: Option<bool>,
    /// Frames before and after, and events, of every `PassPriority` after a concession.
    passes_after_concession: Vec<(Vec<FrameKind>, Vec<FrameKind>, Vec<GameEvent>)>,
}

fn frame_kinds(state: &GameState) -> Vec<FrameKind> {
    state
        .resolution_stack
        .iter()
        .map(|frame| frame.kind())
        .collect()
}

fn adds_counter(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::CounterAdded { .. }))
}

/// Answers every exile prompt with the planned pick (falling back to the only
/// card a seat holds), resolves the reflexive target, and passes priority until
/// the stack and resolution stack are empty.
fn drive_augusta(fixture: &mut PromptedAugusta, picks: [Pick; 4], plan: Plan) -> Drive {
    let mut drive = Drive::default();
    let mut killed = false;
    for _ in 0..150 {
        match fixture.runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                if let Some((who, when)) = plan.concede {
                    if player == when && drive.conceded.is_none() {
                        let result = fixture.runner.act(GameAction::Concede { player_id: who });
                        drive.conceded = Some(result.is_ok());
                        continue;
                    }
                }
                if let Some((who, when)) = plan.kill {
                    if player == when && !killed {
                        killed = true;
                        fixture.runner.state_mut().players[who.0 as usize].life = 0;
                    }
                }
                drive.prompts.push((player, cards.len()));
                drive.shapes.push(frame_kinds(fixture.runner.state()));
                let seat = player.0 as usize;
                let planned = match picks[seat] {
                    Pick::Nonland => fixture.nonland[seat],
                    Pick::Land => fixture.land[seat],
                };
                let pick = planned
                    .or(fixture.nonland[seat])
                    .or(fixture.land[seat])
                    .expect("a prompted seat holds a card");
                let result = fixture
                    .runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("the exile choice is accepted");
                drive.counters_per_answer.push(adds_counter(&result.events));
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                fixture
                    .runner
                    .choose_first_legal_target()
                    .expect("Augusta is a legal attacking target");
            }
            WaitingFor::GameOver { .. } => break,
            _ => {
                let state = fixture.runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                let before = frame_kinds(state);
                let result = fixture
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
                if drive.conceded.is_some() {
                    drive.passes_after_concession.push((
                        before,
                        frame_kinds(fixture.runner.state()),
                        result.events,
                    ));
                }
            }
        }
    }
    drive
}

fn zone_of(fixture: &PromptedAugusta, id: ObjectId) -> Zone {
    fixture.runner.state().objects[&id].zone
}

/// Reach guard: every seat that answered exiled exactly its planned card, and
/// every card a seat did not pick stayed in its owner's graveyard.
fn assert_picks_reached_exile(fixture: &PromptedAugusta, picks: [Pick; 4], seats: &[PlayerId]) {
    for &player in seats {
        let seat = player.0 as usize;
        let (chosen, other) = match picks[seat] {
            Pick::Nonland => (fixture.nonland[seat], fixture.land[seat]),
            Pick::Land => (fixture.land[seat], fixture.nonland[seat]),
        };
        let chosen = chosen.expect("the planned card was staged");
        assert_eq!(
            zone_of(fixture, chosen),
            Zone::Exile,
            "seat {seat}'s pick is exiled"
        );
        if let Some(other) = other {
            assert_eq!(
                zone_of(fixture, other),
                Zone::Graveyard,
                "seat {seat}'s other card stays in the graveyard"
            );
        }
    }
}

fn prompted_counters(picks: [Pick; 4]) -> (u32, Drive, PromptedAugusta) {
    let mut fixture = augusta_with_choices([BOTH; 4], &[]);
    let drive = drive_augusta(&mut fixture, picks, Plan::default());
    assert_eq!(
        drive.prompts,
        ALL_PLAYERS
            .iter()
            .map(|&player| (player, 2))
            .collect::<Vec<_>>(),
        "reach guard: every seat is prompted between its two cards"
    );
    assert_picks_reached_exile(&fixture, picks, &ALL_PLAYERS);
    // CR 607.2a: a ledger tail ("that many") is not a linked-exile reader, so the
    // owner's publication links nothing to Augusta.
    assert!(
        !fixture
            .runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == fixture.augusta),
        "a ledger tail never links the clause's exiles"
    );
    (
        plus_counters(&fixture.runner, fixture.augusta),
        drive,
        fixture,
    )
}

const N: Pick = Pick::Nonland;
const L: Pick = Pick::Land;

/// CR 608.2f + CR 608.2c: four prompted nonland exiles are four counters, placed
/// once after the last seat answers. The clause owner sits under the remaining
/// legs at every non-final prompt and alone at the final one, and nothing is
/// counted before the final answer.
#[test]
fn augusta_paused_choices_all_nonland_count_four() {
    let (counters, drive, _) = prompted_counters([N, N, N, N]);
    assert_eq!(counters, 4, "four nonland cards were exiled this way");
    let owner_and_legs = vec![FrameKind::PlayerScopeClause, FrameKind::AbilityContinuation];
    assert_eq!(
        drive.shapes,
        vec![
            owner_and_legs.clone(),
            owner_and_legs.clone(),
            owner_and_legs,
            vec![FrameKind::PlayerScopeClause],
        ],
        "the owner frame stands below the remaining legs at every prompt"
    );
    assert_eq!(
        drive.counters_per_answer,
        vec![false, false, false, true],
        "the tail resolves once, after the final seat's answer"
    );
}

/// CR 608.2c: three nonland exiles and a final land are three counters.
#[test]
fn augusta_paused_choices_final_land_counts_three() {
    let (counters, _, _) = prompted_counters([N, N, N, L]);
    assert_eq!(counters, 3);
}

/// CR 608.2c: a land on the first seat leaves three nonland exiles.
#[test]
fn augusta_paused_choices_first_land_counts_three() {
    let (counters, _, _) = prompted_counters([L, N, N, N]);
    assert_eq!(counters, 3);
}

/// CR 608.2c: four prompted land exiles place nothing (the negative control).
#[test]
fn augusta_paused_choices_all_land_count_zero() {
    let (counters, _, _) = prompted_counters([L, L, L, L]);
    assert_eq!(counters, 0);
}

/// CR 608.2f: prompted, automatic and empty seats in one clause. P0 is prompted
/// (nonland), P1 exiles its only card automatically, P2 is prompted (land), P3
/// has no card.
#[test]
fn augusta_mixed_paused_automatic_and_empty_seats() {
    let mut fixture = augusta_with_choices([BOTH, NONLAND_ONLY, BOTH, EMPTY], &[]);
    let picks = [N, N, L, N];
    let drive = drive_augusta(&mut fixture, picks, Plan::default());
    assert_eq!(drive.prompts, vec![(P0, 2), (P2, 2)]);
    assert_picks_reached_exile(&fixture, picks, &[P0, P1, P2]);
    assert_eq!(plus_counters(&fixture.runner, fixture.augusta), 2);
}

/// CR 104.3a + CR 800.4a: a player who concedes while another seat's prompt
/// stands leaves the game, and their cards leaving with them are not "exiled
/// this way". The clause still counts the three survivors' nonland exiles.
fn concession_while_other_seat_prompted(hand_count: usize) {
    let mut fixture = augusta_with_choices([BOTH; 4], &[(P3, hand_count)]);
    let drive = drive_augusta(
        &mut fixture,
        [N; 4],
        Plan {
            concede: Some((P3, P1)),
            ..Plan::default()
        },
    );
    assert_eq!(drive.conceded, Some(true), "the concession is accepted");
    assert_eq!(drive.prompts, vec![(P0, 2), (P1, 2), (P2, 2)]);
    assert_picks_reached_exile(&fixture, [N; 4], &[P0, P1, P2]);
    for &card in &fixture.hand {
        assert_eq!(
            zone_of(&fixture, card),
            Zone::Exile,
            "the leaver's hand left the game"
        );
    }
    assert_eq!(plus_counters(&fixture.runner, fixture.augusta), 3);
}

#[test]
fn augusta_concession_mid_clause_is_not_exiled_this_way() {
    concession_while_other_seat_prompted(2);
    concession_while_other_seat_prompted(1);
}

/// CR 104.3a: a seat that already answered and then concedes keeps its answer:
/// that exile happened this way.
#[test]
fn augusta_answered_seat_concession_keeps_its_exile() {
    let mut fixture = augusta_with_choices([BOTH; 4], &[(P1, 1)]);
    let drive = drive_augusta(
        &mut fixture,
        [N; 4],
        Plan {
            concede: Some((P1, P2)),
            ..Plan::default()
        },
    );
    assert_eq!(drive.conceded, Some(true));
    assert_eq!(drive.prompts, vec![(P0, 2), (P1, 2), (P2, 2), (P3, 2)]);
    assert_eq!(plus_counters(&fixture.runner, fixture.augusta), 4);
}

/// CR 104.3a + CR 800.4a: the prompted seat concedes instead of answering. The
/// remaining seats resume on the following `PassPriority`s, inside the owner's
/// resumption brackets, so their exiles still count.
fn prompted_seat_concedes(graveyards: [Graveyard; 4]) -> (PromptedAugusta, Drive) {
    let mut fixture = augusta_with_choices(graveyards, &[]);
    let drive = drive_augusta(
        &mut fixture,
        [N; 4],
        Plan {
            concede: Some((P1, P1)),
            ..Plan::default()
        },
    );
    assert_eq!(drive.conceded, Some(true));
    assert_picks_reached_exile(&fixture, [N; 4], &[P0, P2, P3]);
    assert_eq!(plus_counters(&fixture.runner, fixture.augusta), 3);
    (fixture, drive)
}

#[test]
fn augusta_prompted_seat_concession_counts_the_others() {
    prompted_seat_concedes([BOTH; 4]);
}

#[test]
fn augusta_prompted_seat_concession_with_automatic_later_seats() {
    prompted_seat_concedes([BOTH, BOTH, NONLAND_ONLY, NONLAND_ONLY]);
}

/// The B-mixed shape: after P1 concedes, a `PassPriority` resumes the legs,
/// P2 exiles automatically inside that resumption, and P3's prompt stands with
/// only the owner below it.
#[test]
fn augusta_prompted_seat_concession_mixed_resumes_inside_the_owner() {
    let (fixture, drive) = prompted_seat_concedes([BOTH, BOTH, NONLAND_ONLY, BOTH]);
    let p2_card = fixture.nonland[2].expect("P2's card was staged");
    let resumption = drive
        .passes_after_concession
        .iter()
        .find(|(_, _, events)| {
            events.iter().any(|event| {
                matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. }
                    if *object_id == p2_card)
            })
        })
        .expect("reach guard: a PassPriority resumes P2's automatic exile");
    assert_eq!(
        (resumption.0.clone(), resumption.1.clone()),
        (
            vec![FrameKind::PlayerScopeClause, FrameKind::AbilityContinuation],
            vec![FrameKind::PlayerScopeClause],
        ),
        "the resumption drains the legs above the owner and leaves the owner below P3's prompt"
    );
}

/// CR 608.2d: the "may" sibling. P1 declines (no exile), P2 exiles a land.
#[test]
fn augusta_may_sibling_owner_survives_optional_pauses() {
    let body = "Whenever this creature attacks, each player may exile a card from their \
                graveyard. When one or more nonland cards are exiled this way, put that many \
                +1/+1 counters on target attacking creature.";
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9565);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Augusta May Probe", 2, 2, body)
        .id();
    let mut picks: HashMap<PlayerId, Vec<ObjectId>> = HashMap::new();
    for player in ALL_PLAYERS {
        let nonland = scenario
            .add_creature_to_graveyard(player, "Nonland Card", 1, 1)
            .id();
        let land = scenario.add_land_to_graveyard(player, "Land Card").id();
        let pick = match player.0 {
            1 => vec![],
            2 => vec![land],
            _ => vec![nonland],
        };
        picks.insert(player, pick);
    }
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .expect("the source attacks");
    let (prompts, _) = drive_prompts(&mut runner, &picks);
    assert!(
        prompts.contains(&"Optional(1,false)".to_string())
            && !prompts.iter().any(|prompt| prompt.starts_with("Zone(1,")),
        "reach guard: P1 gets only the optional prompt and declines: {prompts:?}"
    );
    for (player, chosen) in &picks {
        for card in chosen {
            assert_eq!(
                runner.state().objects[card].zone,
                Zone::Exile,
                "{player:?}'s pick is exiled"
            );
        }
    }
    assert_eq!(plus_counters(&runner, source), 2);
}

/// Answers every interactive prompt from `picks` (a seat with no entry accepts an
/// optional and takes the first candidates). Returns each prompt and every
/// event the answers and passes produced, in order.
fn drive_prompts(
    runner: &mut GameRunner,
    picks: &HashMap<PlayerId, Vec<ObjectId>>,
) -> (Vec<String>, Vec<GameEvent>) {
    let mut prompts = Vec::new();
    let mut events = Vec::new();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                let chosen: Vec<ObjectId> = picks
                    .get(&player)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|id| cards.contains(id))
                    .take(count)
                    .collect();
                prompts.push(format!("Discard({},{})", player.0, cards.len()));
                let result = runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("the discard choice is accepted");
                events.extend(result.events);
            }
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } => {
                let mut chosen: Vec<ObjectId> = picks
                    .get(&player)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|id| cards.contains(id))
                    .take(count)
                    .collect();
                if chosen.is_empty() && !up_to {
                    chosen = cards.iter().copied().take(count).collect();
                }
                prompts.push(format!("Zone({},{})", player.0, cards.len()));
                let result = runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("the zone choice is accepted");
                events.extend(result.events);
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                let accept = picks.get(&player).is_none_or(|chosen| !chosen.is_empty());
                prompts.push(format!("Optional({},{accept})", player.0));
                let result = runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("the optional choice is accepted");
                events.extend(result.events);
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a legal target exists");
            }
            WaitingFor::DeclareAttackers { .. } => {
                prompts.push("NoAttack".to_string());
                runner.declare_attackers(&[]).expect("no attack is legal");
            }
            _ => {
                let state = runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
                events.extend(result.events);
            }
        }
    }
    (prompts, events)
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> i64 {
    runner.state().players[player.0 as usize].hand.len() as i64
}

fn graveyard_size(runner: &GameRunner, player: PlayerId) -> i64 {
    runner.state().players[player.0 as usize].graveyard.len() as i64
}

fn stock_libraries(scenario: &mut GameScenario, players: &[PlayerId]) {
    for &player in players {
        for index in 0..5 {
            scenario.add_card_to_library_top(player, &format!("Library Card {index}"));
        }
    }
}

/// Verbatim Oracle text (MTGJSON `Mog, Moogle Warrior`).
const MOG: &str = "Lifelink\nDance — At the beginning of your end step, each player may \
     discard a card. Each player who discarded a card this way draws a card. If a creature \
     card was discarded this way, you create a 1/2 white Moogle creature token with lifelink. \
     Then if a noncreature card was discarded this way, put a +1/+1 counter on each Moogle \
     you control.";

/// Mog at end step; returns (hand deltas, graveyard deltas, every event).
fn mog_end_step(decliner: Option<PlayerId>) -> (Vec<i64>, Vec<i64>, Vec<GameEvent>) {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9565);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Mog, Moogle Warrior", 1, 2, MOG)
        .with_subtypes(vec!["Moogle", "Warrior"])
        .id();
    let mut picks = HashMap::new();
    for player in ALL_PLAYERS {
        let creature = scenario.add_creature_to_hand(player, "Bear", 2, 2).id();
        let instant = scenario.add_spell_to_hand(player, "Shock", true).id();
        let pick = if Some(player) == decliner {
            vec![]
        } else if player.0 % 3 == 0 {
            vec![creature]
        } else {
            vec![instant]
        };
        picks.insert(player, pick);
    }
    stock_libraries(&mut scenario, &ALL_PLAYERS);
    let mut runner = scenario.build();
    let hands: Vec<i64> = ALL_PLAYERS.iter().map(|&p| hand_size(&runner, p)).collect();
    let graveyards: Vec<i64> = ALL_PLAYERS
        .iter()
        .map(|&p| graveyard_size(&runner, p))
        .collect();
    runner.advance_to_end_step();
    // P0's combat offers an attack; decline it and finish advancing.
    for _ in 0..5 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            break;
        }
        runner.declare_attackers(&[]).expect("no attack is legal");
        runner.advance_to_end_step();
    }
    let (prompts, events) = drive_prompts(&mut runner, &picks);
    assert!(
        prompts
            .iter()
            .filter(|prompt| prompt.starts_with("Optional("))
            .count()
            == 4,
        "reach guard: every seat is offered the discard: {prompts:?}"
    );
    let hand_deltas = ALL_PLAYERS
        .iter()
        .enumerate()
        .map(|(seat, &p)| hand_size(&runner, p) - hands[seat])
        .collect();
    let graveyard_deltas = ALL_PLAYERS
        .iter()
        .enumerate()
        .map(|(seat, &p)| graveyard_size(&runner, p) - graveyards[seat])
        .collect();
    (hand_deltas, graveyard_deltas, events)
}

/// CR 608.2c: Mog's nested "who discarded this way draws" clause runs from the
/// owner's completion, after the whole discard clause: every discarder draws one
/// (hand unchanged), and the last discard precedes the first draw. A seat that
/// declines discards nothing. (That seat still draws today, on the phase base as
/// well as here: the separate, pre-existing "each player who discarded" parse
/// follow-up, out of this change's scope, so its hand is not asserted.)
#[test]
fn mog_owner_completion_runs_nested_draw_clause() {
    let (hands, graveyards, events) = mog_end_step(None);
    assert_eq!(
        graveyards,
        vec![1, 1, 1, 1],
        "every seat discarded one card"
    );
    assert_eq!(hands, vec![0, 0, 0, 0], "every discarder drew one card");
    let last_discard = events
        .iter()
        .rposition(|event| matches!(event, GameEvent::Discarded { .. }))
        .expect("reach guard: discards happened");
    let first_draw = events
        .iter()
        .position(|event| matches!(event, GameEvent::CardDrawn { .. }))
        .expect("reach guard: draws happened");
    assert!(
        last_discard < first_draw,
        "every discard precedes the first draw"
    );

    let (_, graveyards, _) = mog_end_step(Some(P2));
    assert_eq!(
        graveyards,
        vec![1, 1, 0, 1],
        "P2 declined and discarded nothing"
    );
}

/// CR 608.2f: an answer given before a save is part of the clause after the
/// restore. The serialized owner (with its recorded prompt) round-trips equal.
#[test]
fn augusta_paused_clause_owner_survives_state_round_trip() {
    let mut fixture = augusta_with_choices([BOTH; 4], &[]);
    let mut round_tripped = false;
    for _ in 0..80 {
        match fixture.runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, .. } => {
                let pick = fixture.nonland[player.0 as usize].expect("staged");
                fixture
                    .runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("the exile choice is accepted");
                if player == P1 && !round_tripped {
                    round_tripped = true;
                    let json = serde_json::to_value(fixture.runner.state()).expect("serializes");
                    let text = json.to_string();
                    assert!(
                        text.contains("PlayerScopeClause"),
                        "the save holds the owner"
                    );
                    assert!(
                        !text.contains("player_scope_linked_exile"),
                        "the retired sidecar is never written"
                    );
                    let restored: GameState = serde_json::from_value(json).expect("restores");
                    assert_eq!(
                        restored.resolution_stack,
                        fixture.runner.state().resolution_stack,
                        "the restored stack equals the live one"
                    );
                    *fixture.runner.state_mut() = restored;
                }
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                fixture
                    .runner
                    .choose_first_legal_target()
                    .expect("Augusta is a legal attacking target");
            }
            _ => {
                let state = fixture.runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                fixture
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
        }
    }
    assert!(
        round_tripped,
        "reach guard: the save was taken after P1's answer"
    );
    assert_eq!(plus_counters(&fixture.runner, fixture.augusta), 4);
}

/// Verbatim Oracle text (MTGJSON `Lightstall Inquisitor`).
const LIGHTSTALL: &str = "Vigilance\nWhen this creature enters, each opponent exiles a card \
     from their hand and may play that card for as long as it remains exiled. Each spell cast \
     this way costs {1} more to cast. Each land played this way enters tapped.";

/// CR 608.2c + CR 608.2f: the printed ETB's "that card" grants each opponent's
/// own exiled card one play permission, prompted or automatic.
#[test]
fn lightstall_inquisitor_grants_every_opponents_exiled_card() {
    for cards_each in [2, 1] {
        let mut scenario = GameScenario::new_n_player(3, 9565);
        scenario.at_phase(Phase::PreCombatMain);
        let inquisitor = scenario
            .add_creature_to_hand_from_oracle(P0, "Lightstall Inquisitor", 2, 1, LIGHTSTALL)
            .id();
        let mut picks = HashMap::new();
        let mut chosen = Vec::new();
        for player in [P1, P2] {
            let card = scenario.add_creature_to_hand(player, "Hand A", 1, 1).id();
            if cards_each > 1 {
                scenario.add_creature_to_hand(player, "Hand B", 1, 1);
            }
            chosen.push(card);
            picks.insert(player, vec![card]);
        }
        let mut runner = scenario.build();
        runner.cast(inquisitor).commit();
        drive_prompts(&mut runner, &picks);
        let outcome: Vec<(Zone, usize)> = chosen
            .iter()
            .map(|card| {
                let object = &runner.state().objects[card];
                (object.zone, object.casting_permissions.len())
            })
            .collect();
        assert_eq!(
            outcome,
            vec![(Zone::Exile, 1), (Zone::Exile, 1)],
            "{cards_each} card(s) each: both exiled cards carry one play permission"
        );
    }
}

/// Non-regression for draw-for-each-discard tails: a synthetic Syphon Mind
/// attack body (2 draws), an "each opponent … land card discarded" body (2) and a
/// card-type count (a land and creatures discarded: 2 draws, so P0's hand ends
/// one card up after its own discard).
#[test]
fn discard_this_way_tails_count_every_seat() {
    let cases: [(&str, u8, i64); 3] = [
        (
            "Whenever this creature attacks, each other player discards a card. You draw a \
             card for each card discarded this way.",
            3,
            2,
        ),
        (
            "Whenever this creature attacks, each opponent discards a card. You draw a card \
             for each land card discarded this way.",
            3,
            2,
        ),
        (
            "Whenever this creature attacks, each player discards a card. Then you draw a card \
             for each card type among cards discarded this way.",
            4,
            2,
        ),
    ];
    for (body, players, expected) in cases {
        let mut scenario = GameScenario::new_n_player(players, 9565);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Discard Probe", 2, 2, body)
            .id();
        let seats: Vec<PlayerId> = (0..players).map(PlayerId).collect();
        let mut picks = HashMap::new();
        for &player in &seats {
            let creature = scenario.add_creature_to_hand(player, "Bear", 2, 2).id();
            let land = scenario.add_land_to_hand(player, "Forest").id();
            let pick = if body.contains("land card discarded") || (players == 4 && player == P0) {
                land
            } else {
                creature
            };
            picks.insert(player, vec![pick]);
        }
        stock_libraries(&mut scenario, &seats);
        let mut runner = scenario.build();
        let before = hand_size(&runner, P0);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(source, AttackTarget::Player(P1))])
            .expect("the source attacks");
        drive_prompts(&mut runner, &picks);
        let discarded_by_p0 = i64::from(players == 4);
        assert_eq!(
            hand_size(&runner, P0) - before + discarded_by_p0,
            expected,
            "{body}"
        );
    }
}

/// CR 704.5a + CR 800.4a: a player who loses to a state-based action mid-clause
/// leaves before answering; their card is not exiled this way.
#[test]
fn augusta_sba_elimination_mid_clause_is_not_exiled_this_way() {
    let mut fixture = augusta_with_choices([BOTH; 4], &[]);
    let drive = drive_augusta(
        &mut fixture,
        [N; 4],
        Plan {
            kill: Some((P3, P1)),
            ..Plan::default()
        },
    );
    assert!(
        fixture.runner.state().players[3].is_eliminated,
        "P3 lost the game"
    );
    assert_eq!(
        drive.prompts,
        vec![(P0, 2), (P1, 2), (P2, 2)],
        "P3 is never prompted"
    );
    assert_picks_reached_exile(&fixture, [N; 4], &[P0, P1, P2]);
    assert_eq!(plus_counters(&fixture.runner, fixture.augusta), 3);
}

/// CR 704.5a: the seat about to be prompted dies when the previous answer's
/// pipeline runs. The answer gate must still open on the next seat's prompt,
/// which the clause's own resumption raised.
#[test]
fn augusta_sba_kills_next_prompted_seat_keeps_counting() {
    for (killed, at) in [(P1, P0), (P2, P1)] {
        let mut fixture = augusta_with_choices([BOTH; 4], &[]);
        let drive = drive_augusta(
            &mut fixture,
            [N; 4],
            Plan {
                kill: Some((killed, at)),
                ..Plan::default()
            },
        );
        assert!(
            fixture.runner.state().players[killed.0 as usize].is_eliminated,
            "{killed:?} lost the game"
        );
        assert!(
            !drive.prompts.iter().any(|(seat, _)| *seat == killed),
            "{killed:?} is never prompted: {:?}",
            drive.prompts
        );
        assert_eq!(
            plus_counters(&fixture.runner, fixture.augusta),
            3,
            "{killed:?} killed"
        );
    }
}

/// Verbatim Oracle text (MTGJSON `Syphon Mind`, `Library of Leng`).
const SYPHON_MIND: &str =
    "Each other player discards a card. You draw a card for each card discarded this way.";
const LIBRARY_OF_LENG: &str = "You have no maximum hand size.\nIf an effect causes you to \
     discard a card, discard it, but you may put it on top of your library instead of into \
     your graveyard.";

struct SyphonOutcome {
    draws: i64,
    prompts: Vec<String>,
}

/// Syphon Mind cast by P0 at a four-player table; `hand_sizes[seat]` cards in
/// each opponent's hand; Library of Leng on every seat in `leng`. Every
/// replacement prompt takes Leng (CR 614.1a: the card is still discarded).
fn syphon_mind_with_leng(hand_sizes: [usize; 4], leng: &[PlayerId]) -> SyphonOutcome {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9565);
    scenario.at_phase(Phase::PreCombatMain);
    let syphon = scenario
        .add_spell_to_hand_from_oracle(P0, "Syphon Mind", false, SYPHON_MIND)
        .id();
    for player in [P1, P2, P3] {
        for index in 0..hand_sizes[player.0 as usize] {
            scenario.add_creature_to_hand(player, &format!("Card {index}"), 1, 1);
        }
        if leng.contains(&player) {
            scenario.add_artifact_from_oracle(player, "Library of Leng", LIBRARY_OF_LENG);
        }
    }
    stock_libraries(&mut scenario, &ALL_PLAYERS);
    let mut runner = scenario.build();
    let before = hand_size(&runner, P0);
    runner.cast(syphon).commit();
    let mut prompts = Vec::new();
    for _ in 0..60 {
        let frames = frame_kinds(runner.state());
        let fan_out = runner
            .state()
            .pending_discard_batch
            .as_ref()
            .is_some_and(|batch| batch.fan_out.is_some());
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice {
                player,
                cards,
                count,
                ..
            } => {
                prompts.push(format!("Discard({},{}) {frames:?}", player.0, cards.len()));
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.iter().copied().take(count).collect(),
                    })
                    .expect("the discard is accepted");
            }
            WaitingFor::ReplacementChoice { player, .. } => {
                prompts.push(format!(
                    "Replacement({}) {frames:?} fan_out={fan_out}",
                    player.0
                ));
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the replacement choice is accepted");
            }
            _ => {
                let state = runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
        }
    }
    SyphonOutcome {
        // Casting Syphon Mind took it from P0's hand.
        draws: hand_size(&runner, P0) - before + 1,
        prompts,
    }
}

const OWNER_OVER_LEGS: &str = "[PlayerScopeClause, AbilityContinuation]";

/// CR 614.1a + CR 608.2c: a later seat's Library of Leng prompt is answered
/// inside the clause, and its discard still counts.
#[test]
fn syphon_mind_later_seat_library_of_leng_draws_for_every_discard() {
    let outcome = syphon_mind_with_leng([0, 2, 1, 1], &[P2]);
    assert!(
        outcome
            .prompts
            .first()
            .is_some_and(|prompt| prompt.starts_with("Discard(1,2)")),
        "reach guard: P1 pauses first on its discard choice: {:?}",
        outcome.prompts
    );
    assert!(
        outcome
            .prompts
            .iter()
            .any(|prompt| prompt.starts_with("Replacement(2)") && prompt.contains(OWNER_OVER_LEGS)),
        "reach guard: P2's Leng prompt stands over the owner and the legs: {:?}",
        outcome.prompts
    );
    assert_eq!(outcome.draws, 3, "three cards were discarded this way");

    let control = syphon_mind_with_leng([0, 2, 1, 1], &[]);
    assert_eq!(control.draws, 3, "the no-Leng control draws three as well");
}

/// CR 608.2f + CR 614.1a: the FIRST pause is a Library of Leng prompt on an
/// automatic discard. The routed clause still parks its owner (not the
/// discard batch's fan-out), which publishes all three discards once.
#[test]
fn syphon_mind_first_seat_library_of_leng_draws_for_every_discard() {
    for leng in [&[P1][..], &[P1, P3][..]] {
        let outcome = syphon_mind_with_leng([0, 1, 2, 1], leng);
        let first = outcome.prompts.first().cloned().unwrap_or_default();
        assert!(
            first.starts_with("Replacement(1)")
                && first.contains(OWNER_OVER_LEGS)
                && first.ends_with("fan_out=false"),
            "reach guard: P1's Leng prompt is raised over the owner and legs, with no \
             discard fan-out: {:?}",
            outcome.prompts
        );
        assert_eq!(
            outcome.draws, 3,
            "Leng on {leng:?}: three cards were discarded"
        );
    }
}

/// Synthetic building block (no printed routed clause has an unless leg): each
/// opponent exiles unless that player pays 2 life.
const UNLESS_EXILE: &str = "Whenever this creature attacks, each opponent exiles a card from \
     their graveyard unless that player pays 2 life. When one or more nonland cards are \
     exiled this way, put that many +1/+1 counters on target attacking creature.";

/// Returns Augusta-shaped counters for the unless leg with each opponent's
/// decision (`true` pays), an optional SBA kill and per-seat land staging.
fn unless_leg(pays: [bool; 4], kill: Option<(PlayerId, PlayerId)>, has_land: [bool; 4]) -> u32 {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9565);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Unless Probe", 2, 2, UNLESS_EXILE)
        .id();
    let mut nonland = Vec::new();
    for player in ALL_PLAYERS {
        nonland.push(
            scenario
                .add_creature_to_graveyard(player, "Nonland Card", 1, 1)
                .id(),
        );
        if has_land[player.0 as usize] {
            scenario.add_land_to_graveyard(player, "Land Card");
        }
    }
    let mut runner = scenario.build();
    // Typed parse guard: exactly one scoped node, and it carries the unless leg.
    let mut scoped = 0;
    let mut scoped_with_unless = 0;
    for entry in runner.state().objects[&source]
        .trigger_definitions
        .iter_unchecked()
    {
        let mut node = entry.definition.execute.as_deref();
        while let Some(definition) = node {
            if definition.player_scope.is_some() {
                scoped += 1;
                if definition.unless_pay.is_some() {
                    scoped_with_unless += 1;
                }
            }
            node = definition.sub_ability.as_deref();
        }
    }
    assert_eq!(
        (scoped, scoped_with_unless),
        (1, 1),
        "the synthetic body parses as intended"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .expect("the source attacks");
    let mut killed = false;
    let mut unless_prompts = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::UnlessPayment { player, .. } => {
                if let Some((who, when)) = kill {
                    if when == player && !killed {
                        killed = true;
                        runner.state_mut().players[who.0 as usize].life = 0;
                    }
                }
                unless_prompts.push(player);
                runner
                    .act(GameAction::PayUnlessCost {
                        pay: pays[player.0 as usize],
                    })
                    .expect("the unless decision is accepted");
            }
            WaitingFor::UnlessPaymentChooseCost { .. } => {
                runner
                    .act(GameAction::ChooseUnlessCostBranch {
                        choice: UnlessCostBranch::Decline,
                    })
                    .expect("declining is accepted");
            }
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                let seat = player.0 as usize;
                let pick = if cards.contains(&nonland[seat]) {
                    nonland[seat]
                } else {
                    cards[0]
                };
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("the exile choice is accepted");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a legal attacking target exists");
            }
            WaitingFor::GameOver { .. } => break,
            _ => {
                let state = runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
        }
    }
    assert!(
        !unless_prompts.is_empty(),
        "reach guard: the unless leg prompts"
    );
    for player in [P1, P2, P3] {
        let seat = player.0 as usize;
        let zone = runner.state().objects[&nonland[seat]].zone;
        if pays[seat] {
            assert_eq!(
                zone,
                Zone::Graveyard,
                "{player:?} paid, so their card stays"
            );
        }
    }
    plus_counters(&runner, source)
}

/// CR 118.12a: the unless answer is clause work; a payer contributes nothing; an
/// SBA-eliminated seat is never prompted.
#[test]
fn unless_leg_owner_counts_every_declining_seat() {
    let all_land = [true; 4];
    assert_eq!(
        unless_leg([false; 4], None, all_land),
        3,
        "every opponent declines"
    );
    assert_eq!(
        unless_leg([false, false, true, false], None, all_land),
        2,
        "P2 pays 2 life"
    );
    assert_eq!(
        unless_leg([false; 4], Some((P3, P1)), all_land),
        2,
        "P3 dies before P1 answers"
    );
    assert_eq!(
        unless_leg([false; 4], Some((P3, P1)), [true, false, true, true]),
        2,
        "P3 dies before P1 answers, P1's exile automatic"
    );
}

/// CR 608.2c "this way": a foreign spell cast while the clause waits at priority
/// (P1 conceded mid-clause) is not the clause's action, even when it exiles a
/// nonland card from a graveyard, and even when it pauses on its own optional
/// prompt.
#[test]
fn augusta_foreign_exile_during_clause_window_is_not_counted() {
    for optional in [false, true] {
        let text = if optional {
            "You may exile target card from a graveyard."
        } else {
            "Exile target card from a graveyard."
        };
        let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9225);
        scenario.at_phase(Phase::PreCombatMain);
        let augusta = scenario
            .add_creature_from_oracle(P0, "Augusta, Order Returned", 2, 2, AUGUSTA)
            .id();
        let mut nonland = Vec::new();
        let mut spells = Vec::new();
        for player in ALL_PLAYERS {
            nonland.push(
                scenario
                    .add_creature_to_graveyard(player, "Nonland Card", 1, 1)
                    .id(),
            );
            scenario.add_land_to_graveyard(player, "Land Card");
            spells.push(
                scenario
                    .add_spell_to_hand_from_oracle(player, "Grave Exile", true, text)
                    .id(),
            );
        }
        let extra = scenario
            .add_creature_to_graveyard(P0, "Extra Nonland", 1, 1)
            .id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(augusta, AttackTarget::Player(P1))])
            .expect("Augusta attacks");
        let mut cast = false;
        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::EffectZoneChoice { player, .. } if player == P1 && !cast => {
                    runner
                        .act(GameAction::Concede { player_id: P1 })
                        .expect("P1 concedes");
                    if let WaitingFor::Priority { player: holder } = runner.state().waiting_for {
                        let spell = spells[holder.0 as usize];
                        let card_id = runner.state().objects[&spell].card_id;
                        runner
                            .act(GameAction::CastSpell {
                                object_id: spell,
                                card_id,
                                targets: vec![],
                                payment_mode: Default::default(),
                            })
                            .expect("the foreign spell is cast in the clause's window");
                        cast = true;
                    }
                }
                WaitingFor::EffectZoneChoice { player, .. } => {
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![nonland[player.0 as usize]],
                        })
                        .expect("the exile choice is accepted");
                }
                WaitingFor::TargetSelection { .. } => {
                    if runner
                        .act(GameAction::SelectTargets {
                            targets: vec![TargetRef::Object(extra)],
                        })
                        .is_err()
                    {
                        runner
                            .choose_first_legal_target()
                            .expect("a legal target exists");
                    }
                }
                WaitingFor::TriggerTargetSelection { .. } => {
                    runner
                        .choose_first_legal_target()
                        .expect("Augusta is a legal attacking target");
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("the foreign optional is accepted");
                }
                _ => {
                    let state = runner.state();
                    if state.stack.is_empty() && state.resolution_stack.is_empty() {
                        break;
                    }
                    runner
                        .act(GameAction::PassPriority)
                        .expect("priority passes");
                }
            }
        }
        assert!(cast, "reach guard: the foreign spell was cast mid-clause");
        assert_eq!(
            runner.state().objects[&extra].zone,
            Zone::Exile,
            "reach guard: the foreign spell exiled a nonland card (optional: {optional})"
        );
        assert_eq!(
            plus_counters(&runner, augusta),
            3,
            "only P0, P2 and P3's clause exiles count (optional: {optional})"
        );
    }
}
