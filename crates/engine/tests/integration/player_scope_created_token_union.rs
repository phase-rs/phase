//! Created-token union across a player-scope fan-out.
//!
//! "Each opponent creates a token …" / "Each player creates …" is ONE
//! instruction whose action is taken for several players (CR 608.2f: processed
//! for each affected player individually, in APNAP order, when it can't be
//! processed simultaneously). The following sentence — "The tokens are goaded
//! …" — is the NEXT instruction (CR 608.2e), and "the tokens" names the result
//! of the previous instruction as a whole (CR 608.2c). So the follow-up applies
//! to every token the instruction created, for every player, never to one
//! player's tokens. Inside the scoped instruction, an in-seat "that token"
//! reference still names only that seat's tokens.
//!
//! Rows (phase-0 plan): V0.1–V0.4b drive the printed cards through the cast
//! pipeline; V0.5–V0.9, V0.17 and V0.18 drive hand-built player-scope chains
//! through the public `resolve_ability_chain` entry and answer every pause with
//! the real `apply()` action; V0.16 records (asserts nothing about haste) the
//! repeat driver's behavior for the adjacent `repeat_for` class.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ContinuousModification, ControllerRef, Duration, Effect,
    PlayerFilter, QuantityExpr, QuantityModification, ReplacementDefinition, ResolvedAbility,
    SubAbilityLink, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::{FrameKind, ResolutionFrame};
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use std::sync::Arc;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Life of the Party (NCC), Scryfall verbatim.
const LIFE_OF_THE_PARTY: &str = "First strike, trample, haste\nWhenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of creatures you control.\nWhen this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";

/// Rendmaw, Creaking Nest (DSC), Scryfall verbatim.
const RENDMAW: &str = "Reach, menace\nWhen Rendmaw enters and whenever you play a card with two or more card types, each player creates a tapped 2/2 black Bird creature token with flying. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";

/// The War Games (WHO), Scryfall verbatim.
const THE_WAR_GAMES: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI — Each player creates three tapped 1/1 white Warrior creature tokens. The tokens are goaded for as long as this Saga remains on the battlefield.\nII, III — Put a +1/+1 counter on each Warrior creature.\nIV — You may exile a nontoken creature you control. When you do, exile all Warriors.";

/// Parallel Lives (ISD), Oracle verbatim.
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";

/// Furygale Flocking, Oracle verbatim (MTGJSON).
const FURYGALE_FLOCKING: &str = "This spell costs {1} less to cast for each instant and sorcery card in your graveyard.\nFor each opponent, create two 3/3 blue and red Elemental creature tokens with flying that attack that opponent this turn if able. They gain haste until end of turn.";

// --- shared helpers -----------------------------------------------------------

/// The goad grants installed on `id`: every transient effect whose affected set
/// is exactly `id` and which grafts `StaticMode::Goaded`.
fn goad_tces(state: &GameState, id: ObjectId) -> Vec<(PlayerId, Duration)> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.affected == TargetFilter::SpecificObject { id }
                && tce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded
                        }
                    )
                })
        })
        .map(|tce| (tce.controller, tce.duration.clone()))
        .collect()
}

fn counters_of(state: &GameState, id: ObjectId, counter: &CounterType) -> u32 {
    state.objects[&id]
        .counters
        .get(counter)
        .copied()
        .unwrap_or(0)
}

/// Battlefield tokens named `name` controlled by `controller`, in id order.
fn tokens_of(state: &GameState, name: &str, controller: PlayerId) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.is_token && obj.name == name && obj.controller == controller
        })
        .collect();
    ids.sort_by_key(|id| id.0);
    ids
}

/// The B1 authority reach-guard: a JSON dump of the state carries a parked
/// player-scope tail authority with a created-token union.
fn tail_authority_parked(state: &GameState) -> bool {
    fn search(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.get("player_scope_tail")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|tail| tail.contains_key("created_tokens"))
                    || map.values().any(search)
            }
            serde_json::Value::Array(items) => items.iter().any(search),
            _ => false,
        }
    }
    search(&serde_json::to_value(state).expect("state serializes"))
}

/// P0.f action-boundary check: no continuation frame is still parked.
fn no_parked_continuation(state: &GameState) -> bool {
    !state
        .resolution_stack
        .iter()
        .any(|frame| matches!(frame, ResolutionFrame::AbilityContinuation(_)))
}

/// P0.g: resolution-stack frame kinds, outer → inner.
fn frame_kinds(state: &GameState) -> Vec<FrameKind> {
    state.resolution_stack.iter().map(|f| f.kind()).collect()
}

fn round_trip(state: &GameState) -> GameState {
    serde_json::from_value(serde_json::to_value(state).expect("pause serializes"))
        .expect("pause restores")
}

/// CR 616.1 instrument: a token-count replacement that applies only to tokens
/// created under `controller`'s control (`token_owner_scope: You`).
fn add_token_count_replacement_for(
    scenario: &mut GameScenario,
    controller: PlayerId,
    name: &str,
    modification: QuantityModification,
) {
    scenario
        .add_creature(controller, name, 0, 4)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .quantity_modification(modification)
                .token_owner_scope(ControllerRef::You),
        );
}

/// The same instrument, installed directly on a hand-built state.
fn install_token_count_replacement(
    state: &mut GameState,
    card_id: u64,
    controller: PlayerId,
    modification: QuantityModification,
) {
    let id = create_object(
        state,
        CardId(card_id),
        controller,
        format!("Token Replacement {card_id}"),
        Zone::Battlefield,
    );
    let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
        .quantity_modification(modification)
        .token_owner_scope(ControllerRef::You);
    let obj = state.objects.get_mut(&id).expect("replacement host exists");
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.base_card_types = obj.card_types.clone();
    obj.replacement_definitions = vec![def.clone()].into();
    obj.base_replacement_definitions = Arc::new(vec![def]);
}

/// Answer every `ReplacementChoice` with the first candidate.
fn answer_replacements(state: &mut GameState, events: &mut Vec<GameEvent>) -> usize {
    let mut answered = 0;
    while let WaitingFor::ReplacementChoice { player, .. } = state.waiting_for.clone() {
        let result = apply(state, player, GameAction::ChooseReplacement { index: 0 })
            .expect("replacement order answer resolves");
        events.extend(result.events);
        answered += 1;
        assert!(answered < 16, "replacement prompts must terminate");
    }
    answered
}

fn runner_answer_replacements(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> usize {
    let mut answered = 0;
    while let WaitingFor::ReplacementChoice { .. } = runner.state().waiting_for {
        let result = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("replacement order answer resolves");
        events.extend(result.events);
        answered += 1;
        assert!(answered < 16, "replacement prompts must terminate");
    }
    answered
}

// --- printed cards through the cast pipeline ------------------------------------

fn cast_life_of_the_party(players: u8) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let life = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(&["first strike", "trample", "haste"], LIFE_OF_THE_PARTY)
        .id();
    let mut runner = scenario.build();
    runner.cast(life).resolve();
    (runner, life)
}

fn cast_rendmaw(players: u8) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let rendmaw = scenario
        .add_creature_to_hand(P0, "Rendmaw, Creaking Nest", 5, 5)
        .with_subtypes(vec!["Scarecrow"])
        .from_oracle_text_with_keywords(&["reach", "menace"], RENDMAW)
        .id();
    let mut runner = scenario.build();
    runner.cast(rendmaw).resolve();
    runner
}

fn single_party_token(state: &GameState, controller: PlayerId) -> ObjectId {
    let tokens = tokens_of(state, "Life of the Party", controller);
    assert_eq!(
        tokens.len(),
        1,
        "REACH-GUARD: {controller:?} must control exactly one Life of the Party token"
    );
    tokens[0]
}

/// V0.1 — CR 608.2c + CR 608.2f: "each opponent creates a token that's a copy
/// of it. The tokens are goaded …" goads EVERY opponent's token, not only the
/// last APNAP seat's.
#[test]
fn life_of_the_party_goads_every_opponents_token() {
    let (runner, life) = cast_life_of_the_party(3);
    let state = runner.state();
    let p1_token = single_party_token(state, P1);
    let p2_token = single_party_token(state, P2);
    assert!(
        tokens_of(state, "Life of the Party", P0).is_empty(),
        "REACH-GUARD: the caster creates no copy"
    );
    assert!(
        goad_tces(state, life).is_empty(),
        "the caster's original Life of the Party is not goaded"
    );
    assert_eq!(
        goad_tces(state, p1_token).len(),
        1,
        "CR 608.2c: P1's token is one of \"the tokens\" and carries the goad"
    );
    assert_eq!(
        goad_tces(state, p2_token).len(),
        1,
        "CR 608.2c: P2's token carries the goad exactly once"
    );
}

/// V0.1 sibling — four players: all three opponents' tokens carry the goad.
#[test]
fn life_of_the_party_goads_all_three_opponents_tokens_in_four_players() {
    let (runner, _life) = cast_life_of_the_party(4);
    let state = runner.state();
    for opponent in [P1, P2, P3] {
        let token = single_party_token(state, opponent);
        assert_eq!(
            goad_tces(state, token).len(),
            1,
            "CR 608.2c: {opponent:?}'s token carries the goad exactly once"
        );
    }
}

/// V0.2 — CR 608.2c + CR 608.2f: Rendmaw's "each player creates a … Bird …
/// The tokens are goaded" goads every player's Bird, the controller's included.
#[test]
fn rendmaw_goads_every_players_bird() {
    let runner = cast_rendmaw(3);
    let state = runner.state();
    for player in [P0, P1, P2] {
        let birds = tokens_of(state, "Bird", player);
        assert_eq!(
            birds.len(),
            1,
            "REACH-GUARD: {player:?} controls exactly one Bird"
        );
        assert!(
            state.objects[&birds[0]].tapped,
            "REACH-GUARD: the Bird entered tapped"
        );
        assert_eq!(
            goad_tces(state, birds[0]).len(),
            1,
            "CR 608.2c: {player:?}'s Bird is one of \"the tokens\" and is goaded exactly once"
        );
    }
}

fn last_created_goad_duration(def: &AbilityDefinition) -> Option<Duration> {
    if let Effect::GenericEffect {
        target: Some(TargetFilter::LastCreated),
        duration,
        ..
    } = &*def.effect
    {
        return duration.clone().or_else(|| def.duration.clone());
    }
    def.sub_ability
        .as_deref()
        .and_then(last_created_goad_duration)
}

/// V0.3 — The War Games chapter I: all nine Warriors (three per player) carry
/// the goad, each with the duration the chapter's follow-up parsed to.
#[test]
fn the_war_games_goads_every_players_warriors() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_spell_to_hand(P0, "The War Games", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(THE_WAR_GAMES)
        .id();
    let mut runner = scenario.build();
    runner.cast(saga).resolve();
    let state = runner.state();

    let parsed_duration = state.objects[&saga]
        .trigger_definitions
        .as_slice()
        .iter()
        .filter_map(|entry| entry.definition.execute.as_deref())
        .find_map(last_created_goad_duration)
        .expect("REACH-GUARD: chapter I's follow-up is a LastCreated goad grant with a duration");

    let mut warriors = 0;
    for player in [P0, P1, P2] {
        let tokens = tokens_of(state, "Warrior", player);
        assert_eq!(
            tokens.len(),
            3,
            "REACH-GUARD: {player:?} controls exactly three Warriors"
        );
        for token in tokens {
            assert!(state.objects[&token].tapped, "REACH-GUARD: tapped Warrior");
            let goads = goad_tces(state, token);
            assert_eq!(
                goads.len(),
                1,
                "CR 608.2c: {player:?}'s Warrior {token:?} is one of \"the tokens\""
            );
            assert_eq!(
                goads[0].1, parsed_duration,
                "the goad lasts exactly as long as the follow-up says"
            );
            warriors += 1;
        }
    }
    assert_eq!(warriors, 9);
}

/// V0.4 — single-seat preservation: in two players Life of the Party's one
/// opponent's token is goaded (base and candidate agree, R0.2).
#[test]
fn life_of_the_party_two_players_single_seat_is_goaded() {
    let (runner, _life) = cast_life_of_the_party(2);
    let state = runner.state();
    let token = single_party_token(state, P1);
    assert_eq!(goad_tces(state, token).len(), 1);
}

/// V0.4b — CR 608.2c: an `All` scope in two players is a multi-seat fan-out;
/// both players' Birds are "the tokens" (R0.3).
#[test]
fn rendmaw_two_players_goads_both_birds() {
    let runner = cast_rendmaw(2);
    let state = runner.state();
    for player in [P0, P1] {
        let birds = tokens_of(state, "Bird", player);
        assert_eq!(birds.len(), 1, "REACH-GUARD: {player:?} controls one Bird");
        assert_eq!(
            goad_tces(state, birds[0]).len(),
            1,
            "CR 608.2c: {player:?}'s Bird is goaded"
        );
    }
}

// --- paused printed-card legs ---------------------------------------------------

/// V0.6 — Rendmaw with a CR 616.1 replacement-order pause at seat 0 (P0's own
/// Birds only). Every Bird of every seat is goaded exactly once after the
/// paused leg, the drained legs and a serde round-trip at the pause.
#[test]
fn rendmaw_paused_at_seat_zero_goads_every_seats_birds() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    add_token_count_replacement_for(
        &mut scenario,
        P0,
        "Token Incrementer",
        QuantityModification::Plus { value: 1 },
    );
    let rendmaw = scenario
        .add_creature_to_hand(P0, "Rendmaw, Creaking Nest", 5, 5)
        .with_subtypes(vec!["Scarecrow"])
        .from_oracle_text_with_keywords(&["reach", "menace"], RENDMAW)
        .id();
    let mut runner = scenario.build();
    runner.cast(rendmaw).resolve();

    // P0.a: the fan-out paused at seat 0, before seats 1 and 2 created anything.
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "REACH-GUARD (P0.a): expected a CR 616.1 ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "REACH-GUARD (P0.a): P0 orders its own replacements"
    );
    assert!(tokens_of(runner.state(), "Bird", P1).is_empty());
    assert!(tokens_of(runner.state(), "Bird", P2).is_empty());
    println!(
        "P0.g V0.6 frames at the seat-0 pause: {:?}",
        frame_kinds(runner.state())
    );

    let mut runner = GameRunner::from_state(round_trip(runner.state()));
    let mut events = Vec::new();
    let answered = runner_answer_replacements(&mut runner, &mut events);
    assert!(answered >= 1);
    let state = runner.state();
    println!(
        "P0.f V0.6 after last answer: waiting_for={:?} frames={:?}",
        state.waiting_for,
        frame_kinds(state)
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "P0.f: the answering action drains the whole clause"
    );
    assert!(
        no_parked_continuation(state),
        "P0.f: no continuation is left parked"
    );

    let p0_birds = tokens_of(state, "Bird", P0);
    assert!(
        p0_birds.len() >= 3,
        "REACH-GUARD: both replacements modified P0's Bird count, got {}",
        p0_birds.len()
    );
    assert_eq!(tokens_of(state, "Bird", P1).len(), 1);
    assert_eq!(tokens_of(state, "Bird", P2).len(), 1);
    for player in [P0, P1, P2] {
        for bird in tokens_of(state, "Bird", player) {
            assert_eq!(
                goad_tces(state, bird).len(),
                1,
                "CR 608.2c: {player:?}'s Bird {bird:?} is one of \"the tokens\""
            );
        }
    }
}

/// V0.8 — Life of the Party's copy route paused at P1's seat (the first
/// opponent) by a CR 616.1 replacement pair on P1's tokens.
#[test]
fn life_of_the_party_paused_copy_route_goads_both_opponents_tokens() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    add_token_count_replacement_for(
        &mut scenario,
        P1,
        "Token Doubler",
        QuantityModification::DOUBLE,
    );
    add_token_count_replacement_for(
        &mut scenario,
        P1,
        "Token Incrementer",
        QuantityModification::Plus { value: 1 },
    );
    let life = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(&["first strike", "trample", "haste"], LIFE_OF_THE_PARTY)
        .id();
    let mut runner = scenario.build();
    runner.cast(life).resolve();

    let paused = matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    );
    println!(
        "P0.e V0.8: paused={paused} waiting_for={:?} frames={:?} authority_parked={}",
        runner.state().waiting_for,
        frame_kinds(runner.state()),
        tail_authority_parked(runner.state())
    );
    let mut authority_at_pause = None;
    let mut events = Vec::new();
    if paused {
        assert!(
            tokens_of(runner.state(), "Life of the Party", P2).is_empty(),
            "REACH-GUARD (P0.a): P2's seat is still pending at P1's pause"
        );
        authority_at_pause = Some(tail_authority_parked(runner.state()));
        runner_answer_replacements(&mut runner, &mut events);
        let state = runner.state();
        println!(
            "P0.f V0.8 after last answer: waiting_for={:?} frames={:?}",
            state.waiting_for,
            frame_kinds(state)
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(no_parked_continuation(state));
    }
    let state = runner.state();
    let p1_tokens = tokens_of(state, "Life of the Party", P1);
    if paused {
        assert!(
            p1_tokens.len() >= 3,
            "REACH-GUARD: both replacements modified P1's copy count"
        );
    }
    let p2_token = single_party_token(state, P2);
    for token in p1_tokens.iter().copied().chain([p2_token]) {
        assert_eq!(
            goad_tces(state, token).len(),
            1,
            "CR 608.2c: token {token:?} is one of \"the tokens\""
        );
    }
    if let Some(parked) = authority_at_pause {
        assert!(
            parked,
            "REACH-GUARD (B1): the tail authority rode the clause frame at the copy-route pause"
        );
    }
}

// --- hand-built chains ------------------------------------------------------------

struct Board {
    state: GameState,
    source: ObjectId,
    copied: ObjectId,
    stale: ObjectId,
}

fn creature(state: &mut GameState, card_id: u64, owner: PlayerId, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).expect("created object exists");
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    obj.power = Some(2);
    obj.toughness = Some(2);
    id
}

/// A board with a copy source, a stack source, and a pre-existing token `x`
/// already named by the created-token ledger (an earlier instruction's token).
fn board(players: u8, seed: u64) -> Board {
    let mut state = GameState::new(FormatConfig::standard(), players, seed);
    let copied = creature(&mut state, 1, P0, "Copied Creature");
    let stale = creature(&mut state, 2, P0, "Earlier Token");
    state.objects.get_mut(&stale).unwrap().is_token = true;
    let source = create_object(
        &mut state,
        CardId(3),
        P0,
        "Fan-out Source".to_string(),
        Zone::Battlefield,
    );
    state.last_created_token_ids = vec![stale];
    for (i, player) in (0..players).map(PlayerId).enumerate() {
        for n in 0..3 {
            create_object(
                &mut state,
                CardId(100 + (i as u64) * 10 + n),
                player,
                format!("Library card {n}"),
                Zone::Library,
            );
        }
    }
    Board {
        state,
        source,
        copied,
        stale,
    }
}

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

fn put_counter_on_last_created(source: ObjectId, counter: CounterType) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: counter,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::LastCreated,
        },
        vec![],
        source,
        P0,
    )
}

fn copy_token_of(source: ObjectId, copied: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: TargetFilter::Any,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
        vec![TargetRef::Object(copied)],
        source,
        P0,
    )
}

/// How the scoped template keeps its in-seat "that token" instruction.
#[derive(Debug, Clone, Copy)]
enum Template {
    /// Mandatory copy; the in-seat +1/+1 is gated on `CurrentScopeSucceeded`.
    Mandatory,
    /// "Each player may …": the optional clause keeps its ungated
    /// `ContinuationStep` +1/+1 in the template.
    Optional,
}

/// Scoped `CopyTokenOf` → in-seat +1/+1 on "that token" → the next
/// instruction `tail`.
fn scoped_copy_chain(
    source: ObjectId,
    copied: ObjectId,
    scope: PlayerFilter,
    template: Template,
    tail: ResolvedAbility,
) -> ResolvedAbility {
    let mut kept = put_counter_on_last_created(source, CounterType::Plus1Plus1);
    kept.condition = match template {
        Template::Mandatory => Some(AbilityCondition::current_scope_succeeded()),
        Template::Optional => None,
    };
    kept.sub_link = SubAbilityLink::ContinuationStep;
    let mut tail = tail;
    tail.sub_link = SubAbilityLink::SequentialSibling;
    kept.sub_ability = Some(Box::new(tail));
    let mut head = copy_token_of(source, copied);
    head.player_scope = Some(scope);
    head.optional = matches!(template, Template::Optional);
    head.sub_ability = Some(Box::new(kept));
    head
}

fn tail_counter(source: ObjectId) -> ResolvedAbility {
    put_counter_on_last_created(source, charge())
}

fn copy_tokens(state: &GameState, controller: PlayerId) -> Vec<ObjectId> {
    tokens_of(state, "Copied Creature", controller)
}

/// V0.5 — CR 608.2c + CR 608.2f (non-paused): the in-seat "that token" sees
/// only its seat's token (one +1/+1 each), and the next instruction sees every
/// seat's token (one charge counter each). The earlier token `x` in the ledger
/// is never part of this clause's tokens.
#[test]
fn non_paused_fan_out_binds_in_seat_per_seat_and_tail_to_union() {
    let Board {
        mut state,
        source,
        copied,
        stale,
    } = board(3, 42);
    let chain = scoped_copy_chain(
        source,
        copied,
        PlayerFilter::All,
        Template::Mandatory,
        tail_counter(source),
    );
    resolve_ability_chain(&mut state, &chain, &mut Vec::new(), 0).expect("fan-out resolves");

    let mut all = Vec::new();
    for player in [P0, P1, P2] {
        let tokens = copy_tokens(&state, player);
        assert_eq!(tokens.len(), 1, "REACH-GUARD: one copy per seat");
        all.extend(tokens);
    }
    for token in &all {
        assert_eq!(
            counters_of(&state, *token, &CounterType::Plus1Plus1),
            1,
            "CR 608.2c: the in-seat \"that token\" names only its seat's token"
        );
    }
    for token in &all {
        assert_eq!(
            counters_of(&state, *token, &charge()),
            1,
            "CR 608.2c + CR 608.2f: the next instruction names every seat's token"
        );
    }
    assert_eq!(counters_of(&state, stale, &CounterType::Plus1Plus1), 0);
    assert_eq!(counters_of(&state, stale, &charge()), 0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Accept,
    Decline,
}

/// Drive the optional fan-out: answer each seat's `OptionalEffectChoice` in
/// order. Returns (prompted players, authority reach-guard at each prompt).
fn answer_optional_seats(
    state: &mut GameState,
    decisions: &[(PlayerId, Decision)],
) -> (Vec<PlayerId>, Vec<bool>) {
    let mut prompted = Vec::new();
    let mut authority = Vec::new();
    for (expected, decision) in decisions {
        let WaitingFor::OptionalEffectChoice { player, .. } = state.waiting_for.clone() else {
            panic!(
                "REACH-GUARD (P0.d): expected {expected:?}'s OptionalEffectChoice, got {:?}",
                state.waiting_for
            );
        };
        prompted.push(player);
        authority.push(tail_authority_parked(state));
        println!(
            "P0.g optional prompt for {player:?}: frames={:?} authority={}",
            frame_kinds(state),
            authority.last().unwrap()
        );
        *state = round_trip(state);
        apply(
            state,
            player,
            GameAction::DecideOptionalEffect {
                accept: *decision == Decision::Accept,
            },
        )
        .expect("optional decision resolves");
    }
    (prompted, authority)
}

fn assert_clause_drained(state: &GameState, label: &str) {
    println!(
        "P0.f {label}: waiting_for={:?} frames={:?}",
        state.waiting_for,
        frame_kinds(state)
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "P0.f ({label}): the last answer drains the clause, got {:?}",
        state.waiting_for
    );
    assert!(
        no_parked_continuation(state),
        "P0.f ({label}): no continuation is left parked"
    );
}

fn run_optional_fan_out(decisions: [Decision; 3]) -> (GameState, ObjectId, Vec<bool>) {
    let Board {
        mut state,
        source,
        copied,
        stale,
    } = board(3, 42);
    let chain = scoped_copy_chain(
        source,
        copied,
        PlayerFilter::All,
        Template::Optional,
        tail_counter(source),
    );
    resolve_ability_chain(&mut state, &chain, &mut Vec::new(), 0).expect("fan-out starts");
    let seats = [P0, P1, P2];
    let (prompted, authority) = answer_optional_seats(
        &mut state,
        &seats
            .iter()
            .copied()
            .zip(decisions)
            .collect::<Vec<(PlayerId, Decision)>>(),
    );
    assert_eq!(
        prompted,
        seats.to_vec(),
        "REACH-GUARD (P0.d): each seat is prompted, in APNAP order"
    );
    assert_clause_drained(&state, &format!("V0.7 {decisions:?}"));
    for (player, decision) in seats.iter().zip(decisions) {
        let expected = usize::from(decision == Decision::Accept);
        assert_eq!(
            copy_tokens(&state, *player).len(),
            expected,
            "REACH-GUARD: {player:?} {decision:?}d its seat"
        );
    }
    (state, stale, authority)
}

fn assert_tail_union(state: &GameState, stale: ObjectId, accepted: &[PlayerId]) {
    for player in accepted {
        let token = copy_tokens(state, *player)[0];
        assert_eq!(
            counters_of(state, token, &CounterType::Plus1Plus1),
            1,
            "CR 608.2c: {player:?}'s in-seat \"that token\" names only its own token"
        );
        assert_eq!(
            counters_of(state, token, &charge()),
            1,
            "CR 608.2c + CR 608.2f: the next instruction names {player:?}'s token too"
        );
    }
    assert_eq!(
        counters_of(state, stale, &charge()),
        0,
        "the earlier instruction's token is never one of this clause's tokens"
    );
    assert_eq!(counters_of(state, stale, &CounterType::Plus1Plus1), 0);
}

/// V0.7 — paused legs, per-seat optional prompts, P1 declines.
#[test]
fn optional_fan_out_middle_decline_tail_names_every_created_token() {
    let (state, stale, authority) =
        run_optional_fan_out([Decision::Accept, Decision::Decline, Decision::Accept]);
    assert_tail_union(&state, stale, &[P0, P2]);
    assert_eq!(
        authority,
        vec![true, true, true],
        "REACH-GUARD (B1): the tail authority is parked at every seat's prompt"
    );
}

/// V0.7 variant — the last seat declines.
#[test]
fn optional_fan_out_last_seat_decline_tail_names_earlier_tokens() {
    let (state, stale, authority) =
        run_optional_fan_out([Decision::Accept, Decision::Accept, Decision::Decline]);
    assert_tail_union(&state, stale, &[P0, P1]);
    assert_eq!(authority, vec![true, true, true]);
}

/// V0.7 variant (baseline exclusion) — seat 0 declines, so after seat 0 the
/// ledger still names the earlier token `x`; `x` must not join the union.
#[test]
fn optional_fan_out_first_seat_decline_excludes_the_earlier_token() {
    let (state, stale, authority) =
        run_optional_fan_out([Decision::Decline, Decision::Accept, Decision::Accept]);
    assert_tail_union(&state, stale, &[P1, P2]);
    assert_eq!(authority, vec![true, true, true]);
}

/// V0.7 all-decline (reach-only): the clause drains; what the tail's referent
/// is when nothing was created is residual R-2 and is not asserted.
#[test]
fn optional_fan_out_all_decline_drains() {
    let (state, _stale, _authority) =
        run_optional_fan_out([Decision::Decline, Decision::Decline, Decision::Decline]);
    for player in [P0, P1, P2] {
        assert!(copy_tokens(&state, player).is_empty());
    }
}

fn run_single_seat_optional(decision: Decision) -> (GameState, ObjectId) {
    let Board {
        mut state,
        source,
        copied,
        stale,
    } = board(2, 43);
    let chain = scoped_copy_chain(
        source,
        copied,
        PlayerFilter::Opponent,
        Template::Optional,
        tail_counter(source),
    );
    resolve_ability_chain(&mut state, &chain, &mut Vec::new(), 0).expect("fan-out starts");
    let (prompted, _authority) = answer_optional_seats(&mut state, &[(P1, decision)]);
    assert_eq!(
        prompted,
        vec![P1],
        "REACH-GUARD: the one seat P1 is prompted"
    );
    assert_clause_drained(&state, &format!("V0.7b {decision:?}"));
    (state, stale)
}

/// V0.7b — single seat on the paused path (R0.7): P1's token carries one
/// +1/+1 and one tail counter.
#[test]
fn single_seat_optional_fan_out_tail_names_the_one_token() {
    let (state, stale) = run_single_seat_optional(Decision::Accept);
    let tokens = copy_tokens(&state, P1);
    assert_eq!(tokens.len(), 1, "REACH-GUARD: P1 accepted");
    assert_tail_union(&state, stale, &[P1]);
}

/// V0.7b decline (reach-only).
#[test]
fn single_seat_optional_fan_out_decline_drains() {
    let (state, _stale) = run_single_seat_optional(Decision::Decline);
    assert!(copy_tokens(&state, P1).is_empty());
}

fn event_position(events: &[GameEvent], pred: impl Fn(&GameEvent) -> bool) -> Option<usize> {
    events.iter().position(pred)
}

fn plus_one_added(events: &[GameEvent], ids: &[ObjectId]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(e, GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, .. }
                if ids.contains(object_id))
        })
        .map(|(i, _)| i)
        .collect()
}

fn token_created_for(events: &[GameEvent], ids: &[ObjectId]) -> Option<usize> {
    event_position(
        events,
        |e| matches!(e, GameEvent::TokenCreated { object_id, .. } if ids.contains(object_id)),
    )
}

/// Resolve a scope-`All` copy chain whose seat 0 pauses on a CR 616.1
/// replacement-order prompt for P0's tokens; answer it. Returns the final
/// state and the full event log.
fn run_seat_zero_copy_pause(
    make_tail: impl Fn(ObjectId) -> ResolvedAbility,
    scope: PlayerFilter,
    players: u8,
    replaced: PlayerId,
) -> (GameState, Vec<GameEvent>, ObjectId, Vec<FrameKind>, bool) {
    let Board {
        mut state,
        source,
        copied,
        stale,
    } = board(players, 44);
    install_token_count_replacement(&mut state, 50, replaced, QuantityModification::DOUBLE);
    install_token_count_replacement(
        &mut state,
        51,
        replaced,
        QuantityModification::Plus { value: 1 },
    );
    let chain = scoped_copy_chain(
        source,
        copied,
        scope,
        Template::Mandatory,
        make_tail(source),
    );
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &chain, &mut events, 0).expect("fan-out starts");
    let paused = matches!(
        state.waiting_for,
        WaitingFor::ReplacementChoice { player, .. } if player == replaced
    );
    let frames = frame_kinds(&state);
    let authority = tail_authority_parked(&state);
    println!(
        "P0.e/P0.g seat pause: paused={paused} waiting_for={:?} frames={frames:?} authority={authority}",
        state.waiting_for
    );
    if paused {
        state = round_trip(&state);
        answer_replacements(&mut state, &mut events);
        assert_clause_drained(&state, "copy-route seat pause");
    }
    (state, events, stale, frames, authority)
}

/// V0.8b — arm G2 with a detached ledger-reading tail: seat 0 pauses with
/// the copy-token owner on top and its in-seat remainder parked inside its
/// child boundary. Every token gets one +1/+1 (in seat) and one tail counter
/// (union), and P0's +1/+1 lands before P1's token exists.
#[test]
fn paused_copy_seat_with_in_seat_remainder_completes_before_next_seat() {
    let (state, events, stale, frames, _authority) =
        run_seat_zero_copy_pause(tail_counter, PlayerFilter::All, 3, P0);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "REACH-GUARD (P0.e): the copy route paused and resumed (frames at pause {frames:?})"
    );
    let p0 = copy_tokens(&state, P0);
    assert!(
        p0.len() >= 3,
        "REACH-GUARD: both replacements modified P0's copy count, got {}",
        p0.len()
    );
    let p1 = copy_tokens(&state, P1);
    let p2 = copy_tokens(&state, P2);
    assert_eq!(p1.len(), 1);
    assert_eq!(p2.len(), 1);
    for token in p0.iter().chain(&p1).chain(&p2) {
        assert_eq!(
            counters_of(&state, *token, &CounterType::Plus1Plus1),
            1,
            "CR 608.2c: in-seat \"that token\" for {token:?}"
        );
        assert_eq!(
            counters_of(&state, *token, &charge()),
            1,
            "CR 608.2c + CR 608.2f: the next instruction names {token:?}"
        );
    }
    assert_eq!(counters_of(&state, stale, &charge()), 0);
    let p0_counters = plus_one_added(&events, &p0);
    let p1_created = token_created_for(&events, &p1).expect("P1's token was created");
    assert!(
        p0_counters.len() == p0.len() && p0_counters.iter().all(|&i| i < p1_created),
        "CR 608.2f: seat 0's in-seat instruction completes before seat 1 begins \
         (P0 +1/+1 at {p0_counters:?}, P1 token at {p1_created})"
    );
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

fn life_gained_by(events: &[GameEvent], player: PlayerId) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::LifeChanged { player_id, amount, .. } if *player_id == player && *amount > 0))
        .map(|(i, _)| i)
        .collect()
}

/// V0.17 — R0.8: a paused clause whose tail does NOT read the ledger (so it is
/// not detached) still processes seat 0's remaining in-seat instruction before
/// seat 1, and the next instruction follows the whole clause.
#[test]
fn paused_seat_remainder_precedes_next_seats_and_non_detached_tail() {
    let (state, events, _stale, frames, _authority) =
        run_seat_zero_copy_pause(gain_one_life, PlayerFilter::All, 3, P0);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "REACH-GUARD (P0.e): paused and resumed (frames at pause {frames:?})"
    );
    let p0 = copy_tokens(&state, P0);
    let p1 = copy_tokens(&state, P1);
    let p2 = copy_tokens(&state, P2);
    assert!(p0.len() >= 3, "REACH-GUARD: replacements applied to P0");
    assert_eq!((p1.len(), p2.len()), (1, 1));
    // The order assertion comes first and is non-vacuous: every P0 token's
    // in-seat counter must exist AND precede seat 1's token.
    let p0_counters = plus_one_added(&events, &p0);
    let p1_created = token_created_for(&events, &p1).expect("P1's token was created");
    assert!(
        p0_counters.len() == p0.len() && p0_counters.iter().all(|&i| i < p1_created),
        "CR 608.2f: seat 0's in-seat instruction (one +1/+1 per P0 token) precedes \
         seat 1 (P0 +1/+1 at {p0_counters:?} for {} tokens, P1 token at {p1_created})",
        p0.len()
    );
    for token in p0.iter().chain(&p1).chain(&p2) {
        assert_eq!(
            plus_one_added(&events, &[*token]).len(),
            1,
            "REACH-GUARD: one in-seat CounterAdded for {token:?}"
        );
    }
    let gains = life_gained_by(&events, P0);
    assert_eq!(gains.len(), 1, "REACH-GUARD: P0 gains 1 life exactly once");
    let p2_counter = plus_one_added(&events, &p2)[0];
    assert!(
        gains[0] > p2_counter,
        "CR 608.2e: the next instruction follows the whole clause \
         (life at {}, P2 +1/+1 at {p2_counter})",
        gains[0]
    );
}

/// V0.17 sibling — one seat (2 players, `Opponent`): P1's in-seat remainder
/// precedes the non-detached tail.
#[test]
fn single_paused_seat_remainder_precedes_non_detached_tail() {
    let (state, events, _stale, frames, _authority) =
        run_seat_zero_copy_pause(gain_one_life, PlayerFilter::Opponent, 2, P1);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "REACH-GUARD (P0.e): P1's seat paused and resumed (frames at pause {frames:?})"
    );
    let p1 = copy_tokens(&state, P1);
    assert!(p1.len() >= 3, "REACH-GUARD: replacements applied to P1");
    for token in &p1 {
        assert_eq!(plus_one_added(&events, &[*token]).len(), 1);
    }
    let gains = life_gained_by(&events, P0);
    assert_eq!(gains.len(), 1);
    let last_counter = *plus_one_added(&events, &p1).last().unwrap();
    assert!(
        last_counter < gains[0],
        "CR 608.2e: P1's in-seat instruction precedes the next instruction \
         (P1 +1/+1 at {last_counter}, life at {})",
        gains[0]
    );
}

/// V0.9 — a scoped clause that creates nothing leaves the ledger naming the
/// earlier instruction's token (R0.4).
#[test]
fn non_producing_clause_leaves_earlier_token_for_follow_up() {
    let Board {
        mut state,
        source,
        copied,
        ..
    } = board(3, 45);
    let haste = ResolvedAbility::new(
        Effect::GenericEffect {
            static_abilities: vec![engine::types::ability::StaticDefinition::continuous()
                .affected(TargetFilter::LastCreated)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])],
            duration: Some(Duration::Permanent),
            target: Some(TargetFilter::LastCreated),
            end_cost: None,
        },
        vec![],
        source,
        P0,
    );
    let mut draw = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    draw.player_scope = Some(PlayerFilter::All);
    draw.sub_link = SubAbilityLink::SequentialSibling;
    let mut haste = haste;
    haste.sub_link = SubAbilityLink::SequentialSibling;
    draw.sub_ability = Some(Box::new(haste));
    let mut copy = copy_token_of(source, copied);
    copy.sub_ability = Some(Box::new(draw));
    let hands_before: Vec<usize> = state.players.iter().map(|p| p.hand.len()).collect();
    resolve_ability_chain(&mut state, &copy, &mut Vec::new(), 0).expect("chain resolves");
    engine::game::layers::evaluate_layers(&mut state);

    for (player, before) in state.players.iter().zip(hands_before) {
        assert_eq!(
            player.hand.len(),
            before + 1,
            "REACH-GUARD: {:?} drew one card in the scoped clause",
            player.id
        );
    }
    let t = copy_tokens(&state, P0);
    assert_eq!(t.len(), 1);
    assert!(
        state.objects[&t[0]].has_keyword(&Keyword::Haste),
        "CR 608.2c: \"it\" still names the earlier instruction's token"
    );
}

/// V0.18 — arm G3: each seat parks only its remainder (no child frame); the
/// clause and its tail drain in the action that answers the last scry.
#[test]
fn scry_fan_out_with_in_seat_draw_drains_in_the_last_answer() {
    let Board {
        mut state, source, ..
    } = board(3, 46);
    let mut kept = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    kept.condition = Some(AbilityCondition::current_scope_succeeded());
    kept.sub_link = SubAbilityLink::ContinuationStep;
    let mut tail = gain_one_life(source);
    tail.sub_link = SubAbilityLink::SequentialSibling;
    kept.sub_ability = Some(Box::new(tail));
    let mut scry = ResolvedAbility::new(
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    scry.player_scope = Some(PlayerFilter::All);
    scry.sub_ability = Some(Box::new(kept));

    let hands_before: Vec<usize> = state.players.iter().map(|p| p.hand.len()).collect();
    resolve_ability_chain(&mut state, &scry, &mut Vec::new(), 0).expect("fan-out starts");
    let mut last_events = Vec::new();
    for expected in [P0, P1, P2] {
        let WaitingFor::ScryChoice { player, cards, .. } = state.waiting_for.clone() else {
            panic!(
                "expected {expected:?}'s ScryChoice, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(player, expected, "REACH-GUARD: scry prompts follow APNAP");
        println!(
            "P0.g V0.18 at {player:?}'s scry: frames={:?}",
            frame_kinds(&state)
        );
        last_events = apply(&mut state, player, GameAction::SelectCards { cards })
            .expect("scry answer resolves")
            .events;
    }
    assert_clause_drained(&state, "V0.18");
    for (player, before) in state.players.iter().zip(hands_before) {
        assert_eq!(
            player.hand.len(),
            before + 1,
            "{:?} drew one card",
            player.id
        );
    }
    let p2_draw = event_position(
        &last_events,
        |e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == P2),
    )
    .expect("P2's draw is in the last action");
    let gain = life_gained_by(&last_events, P0);
    assert_eq!(gain.len(), 1);
    assert!(gain[0] > p2_draw, "the tail follows P2's draw");
}

/// V0.16 — R-4 class-boundary measurement (record only): Furygale Flocking's
/// repeat-driver tokens and their haste, at whatever product code is built.
#[test]
fn record_furygale_flocking_haste_by_iteration() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let flocking = scenario
        .add_spell_to_hand_from_oracle(P0, "Furygale Flocking", false, FURYGALE_FLOCKING)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(flocking).resolve();
    engine::game::layers::evaluate_layers(runner.state_mut());
    let state = runner.state();
    let elementals = tokens_of(state, "Elemental", P0);
    for id in &elementals {
        println!(
            "V0.16 Furygale Flocking: token {id:?} haste={}",
            state.objects[id].has_keyword(&Keyword::Haste)
        );
    }
    println!("V0.16 Furygale Flocking: tokens = {}", elementals.len());
}
