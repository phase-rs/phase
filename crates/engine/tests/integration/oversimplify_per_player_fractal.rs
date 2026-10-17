//! Oversimplify (SOC/C21/DSC/PRM) — per-player Fractal token counter count.
//!
//! Oracle text:
//!   "Exile all creatures. Each player creates a 0/0 green and blue Fractal
//!    creature token and puts a number of +1/+1 counters on it equal to the
//!    total power of creatures they controlled that were exiled this way."
//!
//! Pre-fix bug: the dynamic-quantity clause was silently swallowed at parse
//! time (`SwallowedClause` / `DynamicQty`), so the Fractal token entered with
//! `enter_with_counters: []` and was a 0/0 — instantly dead to SBAs. Four
//! repairs land the card end-to-end:
//!
//!   1. Parser — `try_parse_put_counters_on_token_followup` now accepts the
//!      third-person `puts ` verb AND the `"a number of <type> counters on it
//!      equal to <quantity>"` body (delegating to the shared
//!      `parse_dynamic_counter_suffix_body`).
//!   2. Parser — `parse_type_phrase_folding_with_ctx` lowers
//!      `"creatures they controlled that were exiled this way"` into a
//!      composite `And{Typed{Creature,ScopedPlayer}, ExiledBySource}` filter.
//!   3. Engine — `QuantityRef::Aggregate{Power|Toughness}` falls back to the
//!      LKI snapshot when the matched object is off battlefield, per CR 608.2h
//!      + CR 400.7. Without this, the aggregate returned 0 because
//!        `obj.power` is `None` for non-battlefield objects.
//!   4. Engine — `Typed{controller: ...}` filter evaluation falls back to the
//!      LKI controller for non-battlefield objects, so "they controlled"
//!      semantics work even for stolen creatures (whose post-exile controller
//!      resets to owner).
//!
//! The token is created first and the counters are put on it afterwards
//! (CR 608.2c + CR 122.1): the Token is followed by a `ContinuationStep`
//! `PutCounter { target: LastCreated }` that carries the same player scope.
//! "Each player" governs both actions, so they are processed in the CR 608.2e
//! order: every player creates their Fractal (APNAP), and only then does every
//! player put counters on "it" — the token that player created, i.e. the
//! tokens that player owns (CR 111.2), bound per player when the second action
//! begins and held through any replacement-choice pause.
//!
//! The first test parses the verbatim Oracle text and drives the real resolver
//! (`resolve_ability_chain`) end-to-end: exile all creatures, then verify each
//! player's Fractal token receives the correct counter count equal to that
//! player's total exiled power. The remaining tests cast the card through the
//! real cast pipeline and answer every replacement-choice prompt, recording
//! the board at each prompt.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Oversimplify's verbatim Oracle text (MTGJSON).
const OVERSIMPLIFY: &str = "Exile all creatures. Each player creates a 0/0 green and blue Fractal creature token and puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way.";

/// Verbatim Oracle text (MTGJSON) of the replacement-effect fixtures.
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";

fn add_creature(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    // Ensure the battlefield list reflects the live creature.
    if !state.battlefield.contains(&id) {
        state.battlefield.push_back(id);
    }
    id
}

#[test]
fn oversimplify_per_player_fractal_counters_match_exiled_power() {
    let parsed = parse_oracle_text(
        OVERSIMPLIFY,
        "Oversimplify",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let definition = parsed
        .abilities
        .first()
        .expect("Oversimplify should parse a spell ability");

    // ----- Parser-level assertions (the four-task class repair). -----
    use engine::types::ability::{
        AggregateFunction, ControllerRef, Effect, ObjectProperty, PlayerFilter, QuantityExpr,
        QuantityRef, SubAbilityLink, TargetFilter, TypedFilter,
    };
    let Effect::ChangeZoneAll { .. } = definition.effect.as_ref() else {
        panic!(
            "Oversimplify must start with ChangeZoneAll (exile all creatures), got {:?}",
            definition.effect
        );
    };
    let token_def = definition
        .sub_ability
        .as_deref()
        .expect("Oversimplify chains the per-player Token sub-ability");
    let Effect::Token {
        enter_with_counters,
        ..
    } = token_def.effect.as_ref()
    else {
        panic!(
            "Oversimplify's sub-ability must be a Token effect, got {:?}",
            token_def.effect
        );
    };
    assert!(
        enter_with_counters.is_empty(),
        "the Fractal token receives its counters by a later instruction, not as \
         entry counters: {enter_with_counters:?}"
    );
    let placement = token_def
        .sub_ability
        .as_deref()
        .expect("the counter placement is the Token's direct sub");
    assert_eq!(placement.sub_link, SubAbilityLink::ContinuationStep);
    // CR 608.2e: "Each player" is the subject of both actions, so the placement
    // carries the creation's player scope and runs as the second all-players
    // action rather than inside each player's creation.
    assert_eq!(token_def.player_scope, Some(PlayerFilter::All));
    assert_eq!(placement.player_scope, Some(PlayerFilter::All));
    assert_eq!(placement.player_scope, token_def.player_scope);
    let Effect::PutCounter {
        counter_type,
        count: count_expr,
        target: TargetFilter::LastCreated,
    } = placement.effect.as_ref()
    else {
        panic!(
            "the Token must be followed by PutCounter {{ LastCreated }} — pre-fix \
             the dynamic quantity was swallowed (DynamicQty SwallowedClause), got {:?}",
            placement.effect
        );
    };
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    let expected_count = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                engine::types::ability::CardTypeSetSource::Objects {
                    filter: TargetFilter::And {
                        filters: vec![
                            TargetFilter::Typed(
                                TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                            ),
                            TargetFilter::ExiledBySource,
                        ],
                    },
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    assert_eq!(
        count_expr, &expected_count,
        "the counter quantity must be the composite Aggregate{{Sum, Power, \
         And[Typed{{Creature, ScopedPlayer}}, ExiledBySource]}}"
    );

    // ----- Runtime end-to-end: cast and resolve. -----
    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(1000),
        PlayerId(0),
        "Oversimplify".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&source_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];

    // P0 controls a 4/4 and a 1/1 — total power 5.
    let p0_big = add_creature(&mut state, 101, PlayerId(0), "P0 Big", 4, 4);
    let p0_small = add_creature(&mut state, 102, PlayerId(0), "P0 Small", 1, 1);
    // P1 controls a single 3/3 — total power 3.
    let p1_mid = add_creature(&mut state, 103, PlayerId(1), "P1 Mid", 3, 3);
    // CR 109.4 + CR 608.2h: A *stolen* creature — owned by P0 but currently
    // controlled by P1 (Threaten-style). This exercises Task 4's LKI
    // controller-fallback end-to-end: post-exile, `obj.controller` would
    // reset toward owner (P0), but the look-back filter's "they controlled"
    // scoping must read the at-exile controller (P1) from the LKI cache.
    // Without the fallback, this 2/2 would count toward P0 (yielding P0:7,
    // P1:3) instead of P1 (correct: P0:5, P1:5).
    let stolen = add_creature(&mut state, 104, PlayerId(0), "Stolen 2/2", 2, 2);
    state.objects.get_mut(&stolen).unwrap().controller = PlayerId(1);

    let ability = build_resolved_from_def(definition, source_id, PlayerId(0));
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("Oversimplify must resolve without error");

    // Sanity: every battlefield creature left the battlefield (exiled).
    for victim in [p0_big, p0_small, p1_mid, stolen] {
        assert!(
            !state.battlefield.contains(&victim),
            "creature {victim:?} must be exiled off the battlefield"
        );
    }

    // Find each player's Fractal token. The per-player iteration creates one
    // token per player; the iterating player is the token's controller.
    let p0_token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_token && obj.controller == PlayerId(0) && obj.name == "Fractal")
        .expect("P0 must have a Fractal token on the battlefield");
    let p1_token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_token && obj.controller == PlayerId(1) && obj.name == "Fractal")
        .expect("P1 must have a Fractal token on the battlefield");

    // P0 controlled 4+1 = 5 power. P1 controlled 3 (P1 Mid) + 2 (the stolen
    // P0-owned 2/2) = 5 power — the stolen creature counts toward P1, NOT
    // P0, because "they controlled" reads the at-exile controller via LKI
    // (Task 4). If the LKI controller fallback regresses, this becomes
    // P0:7 / P1:3 and the asserts fail. Each token must also survive SBAs
    // (a 0/0 with N counters is N/N).
    let p0_counters = p0_token
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    let p1_counters = p1_token
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        p0_counters, 5,
        "P0's Fractal must receive 5 +1/+1 counters (4+1 power exiled — \
         the stolen 2/2 must NOT count for P0 since P1 controlled it at exile), \
         got {p0_counters}"
    );
    assert_eq!(
        p1_counters, 5,
        "P1's Fractal must receive 5 +1/+1 counters (3 own + 2 stolen \
         — the stolen P0-owned creature counts toward P1 because P1 was its \
         controller at exile per CR 109.4 LKI), got {p1_counters}"
    );
}

// ---------------------------------------------------------------------------
// CR 608.2e two-action order through the real cast pipeline.
// ---------------------------------------------------------------------------

/// A battlefield token's `(controller, owner, +1/+1 counters)`.
type TokenView = (PlayerId, PlayerId, u32);

/// Every battlefield token named `name`, sorted by `(owner, controller)`.
fn tokens_named(runner: &GameRunner, name: &str) -> Vec<TokenView> {
    let state = runner.state();
    let mut tokens: Vec<TokenView> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.is_token && object.name == name)
        .map(|object| {
            (
                object.controller,
                object.owner,
                object
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
            )
        })
        .collect();
    tokens.sort_by_key(|&(controller, owner, _)| (owner.0, controller.0));
    tokens
}

fn fractals(runner: &GameRunner) -> Vec<TokenView> {
    tokens_named(runner, "Fractal")
}

/// The board at one `ReplacementChoice` prompt, recorded before it is answered.
#[derive(Debug)]
struct Prompt {
    chooser: PlayerId,
    candidates: Vec<String>,
    fractals: Vec<TokenView>,
}

/// Drive the cast to an empty stack. Each `ReplacementChoice` is recorded,
/// then `at_prompt` runs (hostile board edits), then the candidate named by
/// `answer` is chosen. Any other prompt fails the test.
fn drive(
    runner: &mut GameRunner,
    answer: &dyn Fn(&[String]) -> &'static str,
    at_prompt: &mut dyn FnMut(&mut GameRunner, usize),
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return prompts,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|candidate| candidate.source_name.clone())
                    .collect();
                prompts.push(Prompt {
                    chooser: player,
                    candidates: names.clone(),
                    fractals: fractals(runner),
                });
                at_prompt(runner, prompts.len() - 1);
                let wanted = answer(&names);
                let index = names
                    .iter()
                    .position(|name| name == wanted)
                    .unwrap_or_else(|| panic!("{wanted} is not a candidate: {names:?}"));
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("replacement choice accepted");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the cast did not settle");
}

fn no_board_edit(_: &mut GameRunner, _: usize) {}

/// Two players with unequal power on the battlefield: P0 a 2/2, P1 a 3/3.
fn two_player_board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario.add_creature(P1, "Ogre", 3, 3);
    scenario
}

fn with_scales_and_evolution(scenario: &mut GameScenario, players: &[PlayerId]) {
    for &player in players {
        scenario.add_enchantment_from_oracle(player, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(player, "Branching Evolution", BRANCHING_EVOLUTION);
    }
}

/// Cast Oversimplify (P0) through the real cast pipeline.
fn cast_oversimplify(mut scenario: GameScenario) -> GameRunner {
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oversimplify", false, OVERSIMPLIFY)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    runner
}

fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "resolution completed");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn assert_counter_choice(prompt: &Prompt, chooser: PlayerId) {
    assert_eq!(prompt.chooser, chooser, "{prompt:?}");
    let mut candidates = prompt.candidates.clone();
    candidates.sort();
    assert_eq!(
        candidates,
        ["Branching Evolution", "Hardened Scales"],
        "a counter-replacement order choice: {prompt:?}"
    );
}

/// CR 608.2e: at the first counter-replacement prompt, every player has
/// already created their Fractal and none has counters yet.
fn assert_every_fractal_created_before_any_placement(prompt: &Prompt, players: &[PlayerId]) {
    let expected: Vec<TokenView> = players.iter().map(|&player| (player, player, 0)).collect();
    assert_eq!(
        prompt.fractals, expected,
        "every player's Fractal exists, owned and controlled by its creator, with no \
         counters, before any counter placement (CR 608.2e): {prompt:?}"
    );
}

/// Maintainer control (F1): a real two-player cast with unequal power, both
/// players holding Hardened Scales + Branching Evolution. Every player creates
/// before any player places (CR 608.2e), so BOTH Fractals are on the
/// battlefield at the first replacement prompt. Each affected controller orders
/// their own replacements in APNAP order (CR 616.1, CR 101.4): Hardened Scales
/// first gives (n + 1) × 2.
#[test]
fn oversimplify_creates_every_fractal_before_placing_counters_scales_first() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0, P1]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Hardened Scales", &mut no_board_edit);

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_counter_choice(&prompts[1], P1);
    assert_eq!(prompts[1].fractals, vec![(P0, P0, 6), (P1, P1, 0)]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 6), (P1, P1, 8)]);
    assert_settled(&runner);
}

/// The control with the other order: Branching Evolution first gives 2n + 1.
#[test]
fn oversimplify_creates_every_fractal_before_placing_counters_evolution_first() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0, P1]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Branching Evolution", &mut no_board_edit);

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_counter_choice(&prompts[1], P1);
    assert_eq!(prompts[1].fractals, vec![(P0, P0, 5), (P1, P1, 0)]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 5), (P1, P1, 7)]);
    assert_settled(&runner);
}

/// The pending per-player placements survive a whole-state serialization round
/// trip at the first prompt: each remaining leg keeps its bound Fractal.
#[test]
fn oversimplify_per_player_binding_survives_serialization_at_the_prompt() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0, P1]);
    let mut runner = cast_oversimplify(scenario);

    let mut round_trips = 0;
    let prompts = drive(
        &mut runner,
        &|_| "Hardened Scales",
        &mut |runner, prompt| {
            if prompt == 0 {
                let wire = serde_json::to_value(runner.state()).expect("state serializes");
                *runner.state_mut() = serde_json::from_value(wire).expect("state deserializes");
                round_trips += 1;
            }
        },
    );

    assert_eq!(
        round_trips, 1,
        "the round trip happened at the first prompt"
    );
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_counter_choice(&prompts[1], P1);
    assert_eq!(fractals(&runner), vec![(P0, P0, 6), (P1, P1, 8)]);
    assert_settled(&runner);
}

/// Three players: the counter choices run in APNAP order across more than one
/// remaining player (CR 101.4, CR 616.1), each bound to its own Fractal.
#[test]
fn oversimplify_three_players_place_counters_in_apnap_order() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Squire", 1, 1);
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.add_creature(p2, "Ogre", 3, 3);
    with_scales_and_evolution(&mut scenario, &[P0, P1, p2]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Hardened Scales", &mut no_board_edit);

    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1, p2]);
    for (prompt, chooser) in prompts.iter().zip([P0, P1, p2]) {
        assert_counter_choice(prompt, chooser);
    }
    assert_eq!(
        fractals(&runner),
        vec![(P0, P0, 4), (P1, P1, 6), (p2, p2, 8)]
    );
    assert_settled(&runner);
}

/// Only P0 holds the counter replacements: one prompt, and P1's Fractal
/// already exists when P0 chooses.
#[test]
fn oversimplify_single_affected_controller_sees_every_fractal() {
    let mut scenario = two_player_board();
    with_scales_and_evolution(&mut scenario, &[P0]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Hardened Scales", &mut no_board_edit);

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_every_fractal_created_before_any_placement(&prompts[0], &[P0, P1]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 6), (P1, P1, 3)]);
    assert_settled(&runner);
}

/// Halving Season (P0) + Parallel Lives (P1): P1's creation pauses on an
/// order choice (CR 616.1).
fn creation_pause_board() -> GameScenario {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    scenario
}

/// P1's creation-order prompt, at which P0's Fractal must carry no counters:
/// no player places counters until every player has created (CR 608.2e).
fn assert_creation_choice(prompt: &Prompt) {
    assert_eq!(
        prompt.chooser, P1,
        "P1 orders its creation replacements: {prompt:?}"
    );
    let mut candidates = prompt.candidates.clone();
    candidates.sort();
    assert_eq!(
        candidates,
        ["Halving Season", "Parallel Lives"],
        "{prompt:?}"
    );
    assert_eq!(
        prompt.fractals,
        vec![(P0, P0, 0)],
        "P0 has created and placed nothing yet while P1 is still creating: {prompt:?}"
    );
}

/// A pause inside the first action: every placement waits for P1's creation.
/// Finals: P0 2; P1 creates 1 → 2 (Parallel Lives) → 1 (Halving Season) and
/// puts half of 3, rounded down, on it.
#[test]
fn oversimplify_creation_pause_defers_every_placement() {
    let mut runner = cast_oversimplify(creation_pause_board());

    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut no_board_edit);

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 2), (P1, P1, 1)]);
    assert_settled(&runner);
}

/// Both actions pause: P1's creation choice comes first, then P0's counter
/// choice — with both Fractals present and neither carrying counters.
#[test]
fn oversimplify_pauses_in_both_actions_in_rules_order() {
    let mut scenario = creation_pause_board();
    with_scales_and_evolution(&mut scenario, &[P0]);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(
        &mut runner,
        &|names| {
            if names.iter().any(|name| name == "Parallel Lives") {
                "Parallel Lives"
            } else {
                "Branching Evolution"
            }
        },
        &mut no_board_edit,
    );

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    assert_counter_choice(&prompts[1], P0);
    assert_every_fractal_created_before_any_placement(&prompts[1], &[P0, P1]);
    assert_eq!(fractals(&runner), vec![(P0, P0, 5), (P1, P1, 1)]);
    assert_settled(&runner);
}

fn p0_fractal(runner: &GameRunner) -> ObjectId {
    let state = runner.state();
    *state
        .battlefield
        .iter()
        .find(|id| {
            let object = &state.objects[*id];
            object.is_token && object.name == "Fractal" && object.owner == P0
        })
        .expect("P0's Fractal is on the battlefield")
}

/// "It" is the token the player created — the one they own (CR 111.2) — not
/// whatever they control when the counters are placed: P0's Fractal changes
/// control to P1 between the two actions and still gets P0's count.
#[test]
fn oversimplify_binds_each_player_to_the_fractal_it_owns() {
    let mut runner = cast_oversimplify(creation_pause_board());

    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut |runner, prompt| {
        if prompt == 0 {
            let fractal = p0_fractal(runner);
            let object = runner.state_mut().objects.get_mut(&fractal).unwrap();
            object.controller = P1;
            object.base_controller = Some(P1);
        }
    });

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    assert_eq!(
        fractals(&runner),
        vec![(P1, P0, 2), (P1, P1, 1)],
        "the P0-owned, P1-controlled Fractal gets P0's 2; P1's own gets P1's 1"
    );
    assert_settled(&runner);
}

/// A player whose creation was replaced with nothing has no "it": that
/// player's placement does nothing (CR 609.3), and no other token gets it.
#[test]
fn oversimplify_player_without_a_fractal_places_nothing() {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "unused", &mut no_board_edit);

    assert!(prompts.is_empty(), "{prompts:?}");
    assert_eq!(
        fractals(&runner),
        vec![(P0, P0, 2)],
        "P0's Fractal gets exactly P0's count; P1 created none"
    );
    let state = runner.state();
    let countered: Vec<&str> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.counters.values().any(|&count| count > 0))
        .map(|object| object.name.as_str())
        .collect();
    assert_eq!(countered, ["Fractal"], "no other permanent has counters");
    assert_settled(&runner);
}

/// A Fractal that leaves the battlefield between the two actions is a new
/// object (CR 400.7); its player's placement finds nothing, and the other
/// player's placement is unaffected.
#[test]
fn oversimplify_fractal_gone_before_placement_gets_nothing() {
    let mut runner = cast_oversimplify(creation_pause_board());

    let mut moved = None;
    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut |runner, prompt| {
        if prompt == 0 {
            let fractal = p0_fractal(runner);
            let mut events = Vec::new();
            move_to_zone(runner.state_mut(), fractal, Zone::Graveyard, &mut events);
            moved = Some(fractal);
        }
    });

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_creation_choice(&prompts[0]);
    let moved = moved.expect("the move happened at the prompt");
    assert!(
        !runner.state().battlefield.contains(&moved),
        "P0's Fractal left the battlefield"
    );
    assert_eq!(fractals(&runner), vec![(P1, P1, 1)]);
    assert_settled(&runner);
}

/// Tokens a replacement creates instead are the tokens the player created
/// (CR 614.1a): Divine Visitation + Parallel Lives turn P0's Fractal into two
/// Angels, and "it" is both of them.
#[test]
fn oversimplify_substituted_tokens_are_it() {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE_VISITATION);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = cast_oversimplify(scenario);

    let prompts = drive(&mut runner, &|_| "Parallel Lives", &mut no_board_edit);

    assert!(prompts.is_empty(), "{prompts:?}");
    assert_eq!(
        tokens_named(&runner, "Angel"),
        vec![(P0, P0, 2), (P0, P0, 2)],
        "two Angels under P0, each with P0's count"
    );
    assert_eq!(fractals(&runner), vec![(P1, P1, 3)]);
    assert_settled(&runner);
}

/// A pause in the second action leaves a seat with no Fractal as a REMAINING
/// leg: P0's counter-order choice parks P1's placement, and P1 created none
/// (Halving Season halves P1's one Fractal to zero), so that queued leg has no
/// "it" and does nothing (CR 609.3) instead of putting P1's count on P0's
/// Fractal. P0's Fractal gets P0's 2 with Hardened Scales first:
/// (2 + 1) × 2 = 6 (Halving Season halves only an opponent's counters).
#[test]
fn oversimplify_remaining_player_without_a_fractal_places_nothing_after_a_pause() {
    let mut scenario = two_player_board();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    with_scales_and_evolution(&mut scenario, &[P0]);
    let mut runner = cast_oversimplify(scenario);

    // Any further counter prompt (e.g. one for P1 placing on P0's Fractal) is
    // answered too, so the prompt count below is what reports it.
    let prompts = drive(
        &mut runner,
        &|names| {
            if names.iter().any(|name| name == "Hardened Scales") {
                "Hardened Scales"
            } else {
                "Branching Evolution"
            }
        },
        &mut no_board_edit,
    );

    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_counter_choice(&prompts[0], P0);
    assert_eq!(
        prompts[0].fractals,
        vec![(P0, P0, 0)],
        "P0 pauses on its own placement with P1's leg still remaining, and P1 \
         created no Fractal: {prompts:?}"
    );
    assert_eq!(
        fractals(&runner),
        vec![(P0, P0, 6)],
        "P0's Fractal gets exactly P0's count; P1 created none"
    );
    let state = runner.state();
    let countered: Vec<&str> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.counters.values().any(|&count| count > 0))
        .map(|object| object.name.as_str())
        .collect();
    assert_eq!(countered, ["Fractal"], "no other permanent has counters");
    assert_settled(&runner);
}
