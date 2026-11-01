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

// ---------------------------------------------------------------------------
// Constructed co-scoped chains: which seats a `LastCreated` placement binds
// (CR 608.2e + CR 111.2), and what an empty seat's continuation does.
// ---------------------------------------------------------------------------

/// Constructed (synthetic) chains: "Each player creates a 1/1 Fractal token and
/// [may] put a +1/+1 counter on it[, …]", with the following instruction
/// varied. No printed card carries these shapes; they pin the per-seat binding
/// contract of the player-scope driver.
///
/// - The partition is proven only for the immediately preceding co-scoped
///   token clause (CR 608.2e + CR 111.2: "it" is the token that player
///   created, which that player owns); any other producer leaves the
///   placement reading the full latest-created set.
/// - A seat that created no token performed no placement (CR 609.3), so its
///   own "if you do" / "who doesn't" rider and "otherwise" leg read its own
///   outcome (CR 118.12, CR 608.2c), synchronously and after a pause.
/// - The instruction printed after the clause runs once after the last seat
///   (CR 608.2c), whatever link it was parsed with.
mod constructed {
    use engine::game::effects::resolve_ability_chain;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, ControllerRef, Effect, EffectOutcomeSignal, PlayerFilter,
        PtValue, QuantityExpr, ResolvedAbility, SubAbilityLink, TargetFilter, TypeFilter,
        TypedFilter, UnlessPayModifier,
    };
    use engine::types::actions::GameAction;
    use engine::types::counter::CounterType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaColor, ManaCost};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use super::{BRANCHING_EVOLUTION, HALVING_SEASON, HARDENED_SCALES, PARALLEL_LIVES};

    const P2: PlayerId = PlayerId(2);

    fn fractal_token() -> Effect {
        Effect::Token {
            attack_destination: engine::types::ability::TokenAttackDestination::ControllerChooses,
            name: "Fractal".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string(), "Fractal".to_string()],
            colors: vec![ManaColor::Green],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        }
    }

    fn gain(amount: i32, source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )
    }

    fn placement(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::LastCreated,
            },
            vec![],
            source,
            P0,
        )
    }

    /// The retained continuation after the placement.
    #[derive(Clone, Copy)]
    enum Rider {
        /// "Each player who doesn't gains 1 life."
        WhoDoesnt,
        /// "If they do, they gain 1 life."
        IfYouDo,
        /// "… and gain 1 life" (a dependent continuation of the same sentence).
        AndGain,
        None,
    }

    /// The board: who holds which replacement enchantment.
    #[derive(Clone, Copy)]
    enum Board {
        Empty,
        HalvingP0,
        HalvingP1,
        HalvingP2ParallelP0,
        ParallelP0,
        ScalesEvolutionP0,
    }

    /// The placement's condition.
    #[derive(Clone, Copy)]
    enum Gate {
        None,
        /// "If it's your turn" — true for the active seat only.
        YourTurn,
        /// "If you control an enchantment" — true for a seat holding one.
        ControlsEnchantment,
    }

    #[derive(Clone, Copy)]
    struct Row {
        players: u8,
        board: Board,
        active: PlayerId,
        optional: bool,
        gate: Gate,
        /// "Otherwise, gain 2 life."
        otherwise: bool,
        rider: Rider,
        /// The unscoped "You gain 3 life." printed after the clause, and the
        /// link it was parsed with.
        tail: Option<SubAbilityLink>,
        /// The seat that declines its optional placement, if any.
        decline: Option<PlayerId>,
    }

    impl Row {
        fn new(players: u8, board: Board) -> Self {
            Row {
                players,
                board,
                active: P0,
                optional: false,
                gate: Gate::None,
                otherwise: false,
                rider: Rider::None,
                tail: None,
                decline: None,
            }
        }
        fn optional(self) -> Self {
            Row {
                optional: true,
                ..self
            }
        }
        fn gate(self, gate: Gate) -> Self {
            Row { gate, ..self }
        }
        fn otherwise(self) -> Self {
            Row {
                otherwise: true,
                ..self
            }
        }
        fn rider(self, rider: Rider) -> Self {
            Row { rider, ..self }
        }
        fn tail(self, link: SubAbilityLink) -> Self {
            Row {
                tail: Some(link),
                ..self
            }
        }
        fn active(self, active: PlayerId) -> Self {
            Row { active, ..self }
        }
        fn decline(self, seat: PlayerId) -> Self {
            Row {
                decline: Some(seat),
                ..self
            }
        }
    }

    fn condition(gate: Gate) -> Option<AbilityCondition> {
        match gate {
            Gate::None => None,
            Gate::YourTurn => Some(AbilityCondition::IsYourTurn),
            Gate::ControlsEnchantment => Some(AbilityCondition::ControllerControlsMatching {
                filter: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
                ),
            }),
        }
    }

    /// `Token { Fractal, All }` → `PutCounter { LastCreated, All }` (with the
    /// row's optionality, condition and "otherwise" leg) → the row's rider →
    /// the row's trailing instruction.
    fn chain(source: ObjectId, row: &Row) -> ResolvedAbility {
        let mut token = ResolvedAbility::new(fractal_token(), vec![], source, P0);
        token.player_scope = Some(PlayerFilter::All);
        let mut place = placement(source);
        place.player_scope = Some(PlayerFilter::All);
        place.sub_link = SubAbilityLink::ContinuationStep;
        place.optional = row.optional;
        place.condition = condition(row.gate);
        if row.otherwise {
            place.else_ability = Some(Box::new(gain(2, source)));
        }
        let tail = row.tail.map(|link| {
            let mut tail = gain(3, source);
            tail.sub_link = link;
            Box::new(tail)
        });
        let performed = AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        };
        place.sub_ability = match row.rider {
            Rider::None => tail,
            rider => {
                let mut continuation = gain(1, source);
                match rider {
                    Rider::WhoDoesnt => {
                        continuation.condition = Some(AbilityCondition::Not {
                            condition: Box::new(performed),
                        });
                        continuation.sub_link = SubAbilityLink::SequentialSibling;
                    }
                    Rider::IfYouDo => {
                        continuation.condition = Some(performed);
                        continuation.sub_link = SubAbilityLink::SequentialSibling;
                    }
                    Rider::AndGain => continuation.sub_link = SubAbilityLink::ContinuationStep,
                    Rider::None => unreachable!(),
                }
                continuation.sub_ability = tail;
                Some(Box::new(continuation))
            }
        };
        token.sub_ability = Some(Box::new(place));
        token
    }

    /// The settled board of one row.
    #[derive(Debug)]
    struct Outcome {
        /// Each prompt answered, in order: `(seat, "optional" | "replacement")`.
        prompts: Vec<(PlayerId, &'static str)>,
        /// Each Fractal's `(owner, +1/+1 counters)`, sorted.
        fractals: Vec<(PlayerId, u32)>,
        /// Each player's life change.
        life: Vec<i32>,
    }

    fn fractals(runner: &GameRunner) -> Vec<(PlayerId, u32)> {
        let state = runner.state();
        let mut fractals: Vec<(PlayerId, u32)> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.name == "Fractal")
            .map(|object| {
                (
                    object.owner,
                    object
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                )
            })
            .collect();
        fractals.sort_by_key(|&(owner, counters)| (owner.0, counters));
        fractals
    }

    /// Answer every prompt until the resolution settles. Replacement order:
    /// Hardened Scales first when offered, otherwise the first non-halving
    /// candidate (Parallel Lives before Halving Season). `at_first_prompt` runs
    /// once, at the first prompt, before it is answered.
    fn drive(
        runner: &mut GameRunner,
        decline: Option<PlayerId>,
        at_first_prompt: &mut dyn FnMut(&mut GameRunner),
    ) -> Vec<(PlayerId, &'static str)> {
        let mut prompts = Vec::new();
        for _ in 0..100 {
            let at_prompt = !matches!(runner.state().waiting_for, WaitingFor::Priority { .. });
            if at_prompt && prompts.is_empty() {
                at_first_prompt(runner);
            }
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => return prompts,
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    prompts.push((player, "optional"));
                    runner
                        .act(GameAction::DecideOptionalEffect {
                            accept: decline != Some(player),
                        })
                        .expect("optional answer accepted");
                }
                WaitingFor::ReplacementChoice {
                    player, candidates, ..
                } => {
                    prompts.push((player, "replacement"));
                    let index = candidates
                        .iter()
                        .position(|candidate| candidate.description.contains("plus one"))
                        .or_else(|| {
                            candidates
                                .iter()
                                .position(|candidate| !candidate.description.contains("half"))
                        })
                        .unwrap_or(0);
                    runner
                        .act(GameAction::ChooseReplacement { index })
                        .expect("replacement choice accepted");
                }
                WaitingFor::UnlessPayment { player, .. } => {
                    prompts.push((player, "unless"));
                    runner
                        .act(GameAction::PayUnlessCost { pay: false })
                        .expect("unless answer accepted");
                }
                other => panic!("unexpected prompt {other:?}"),
            }
        }
        panic!("the resolution did not settle");
    }

    fn scenario(players: u8, board: Board) -> GameScenario {
        let mut scenario = if players == 2 {
            GameScenario::new()
        } else {
            GameScenario::new_n_player(players, 42)
        };
        scenario.at_phase(Phase::PreCombatMain);
        let mut add = |player: PlayerId, name: &str, text: &str| {
            scenario.add_enchantment_from_oracle(player, name, text);
        };
        match board {
            Board::Empty => {}
            Board::HalvingP0 => add(P0, "Halving Season", HALVING_SEASON),
            Board::HalvingP1 => add(P1, "Halving Season", HALVING_SEASON),
            Board::HalvingP2ParallelP0 => {
                add(P2, "Halving Season", HALVING_SEASON);
                add(P0, "Parallel Lives", PARALLEL_LIVES);
            }
            Board::ParallelP0 => add(P0, "Parallel Lives", PARALLEL_LIVES),
            Board::ScalesEvolutionP0 => {
                add(P0, "Hardened Scales", HARDENED_SCALES);
                add(P0, "Branching Evolution", BRANCHING_EVOLUTION);
            }
        }
        scenario
    }

    /// Resolve `chain` through the production resolver and settle it.
    fn settle(
        players: u8,
        board: Board,
        active: PlayerId,
        decline: Option<PlayerId>,
        chain: &dyn Fn(ObjectId) -> ResolvedAbility,
        at_first_prompt: &mut dyn FnMut(&mut GameRunner),
    ) -> Outcome {
        let mut runner = scenario(players, board).build();
        runner.state_mut().active_player = active;
        let source = create_object(
            runner.state_mut(),
            CardId(900),
            P0,
            "Constructed source".to_string(),
            Zone::Battlefield,
        );
        let before: Vec<i32> = runner.state().players.iter().map(|p| p.life).collect();
        resolve_ability_chain(runner.state_mut(), &chain(source), &mut Vec::new(), 0)
            .expect("chain resolves");
        let prompts = drive(&mut runner, decline, at_first_prompt);
        let life = runner
            .state()
            .players
            .iter()
            .zip(&before)
            .map(|(player, before)| player.life - before)
            .collect();
        Outcome {
            prompts,
            fractals: fractals(&runner),
            life,
        }
    }

    fn run(row: Row) -> Outcome {
        settle(
            row.players,
            row.board,
            row.active,
            row.decline,
            &|source| chain(source, &row),
            &mut |_| {},
        )
    }

    const LINKS: [SubAbilityLink; 2] = [
        SubAbilityLink::ContinuationStep,
        SubAbilityLink::SequentialSibling,
    ];

    // --- N5: the partition needs the matching co-scoped producer ------------

    /// An unscoped P0 `Token { Fractal }` followed by
    /// `PutCounter { LastCreated, scope All }`.
    fn unscoped_creation_then_each_player_placement(source: ObjectId) -> ResolvedAbility {
        let mut token = ResolvedAbility::new(fractal_token(), vec![], source, P0);
        let mut place = placement(source);
        place.player_scope = Some(PlayerFilter::All);
        place.sub_link = SubAbilityLink::ContinuationStep;
        token.sub_ability = Some(Box::new(place));
        token
    }

    /// N5-1 — the producer is not a co-scoped token clause, so both seats'
    /// instructions address the full latest-created set: P0's one Fractal.
    #[test]
    fn unscoped_creation_then_each_player_placement_reads_the_full_set() {
        let outcome = settle(
            2,
            Board::Empty,
            P0,
            None,
            &unscoped_creation_then_each_player_placement,
            &mut |_| {},
        );
        assert!(outcome.prompts.is_empty());
        assert_eq!(outcome.fractals, vec![(P0, 2)], "both seats' placements");
    }

    /// N5-2 — the same with a pause on P0's leg (CR 616.1: P0 controls the
    /// Fractal, so P0 orders both placements' replacements), and a whole-state
    /// serialization round trip at the first prompt.
    #[test]
    fn unscoped_creation_then_each_player_placement_survives_a_pause() {
        let mut first_prompt_fractals = None;
        let outcome = settle(
            2,
            Board::ScalesEvolutionP0,
            P0,
            None,
            &unscoped_creation_then_each_player_placement,
            &mut |runner| {
                first_prompt_fractals = Some(fractals(runner));
                let wire = serde_json::to_value(runner.state()).expect("state serializes");
                *runner.state_mut() = serde_json::from_value(wire).expect("state deserializes");
            },
        );
        assert_eq!(
            first_prompt_fractals,
            Some(vec![(P0, 0)]),
            "reach guard: the first prompt is P0's leg, before any placement"
        );
        assert_eq!(
            outcome.prompts,
            vec![(P0, "replacement"), (P0, "replacement")]
        );
        assert_eq!(outcome.fractals, vec![(P0, 8)], "(1 + 1) × 2, twice");
    }

    // --- B2: an empty seat keeps its continuation ---------------------------

    /// N5-3 — P0 Halving Season, so P1 creates no Fractal; P0 accepts its
    /// placement. P1 put no counter, so its "who doesn't" rider gives it 1 life.
    #[test]
    fn matched_seat_without_a_token_keeps_its_continuation_after_a_pause() {
        let outcome = run(Row::new(2, Board::HalvingP0)
            .optional()
            .rider(Rider::WhoDoesnt));
        assert_eq!(outcome.prompts, vec![(P0, "optional")]);
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![0, 1]);
    }

    /// N5-4 — synchronous, the empty seat first in APNAP order.
    #[test]
    fn matched_seat_without_a_token_keeps_its_continuation_first_in_apnap() {
        let outcome = run(Row::new(2, Board::HalvingP1).rider(Rider::WhoDoesnt));
        assert!(outcome.prompts.is_empty());
        assert_eq!(outcome.fractals, vec![(P1, 1)]);
        assert_eq!(outcome.life, vec![1, 0]);
    }

    /// N5-5 — synchronous, the empty seat second.
    #[test]
    fn matched_seat_without_a_token_keeps_its_continuation_last_in_apnap() {
        let outcome = run(Row::new(2, Board::HalvingP0).rider(Rider::WhoDoesnt));
        assert!(outcome.prompts.is_empty());
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![0, 1]);
    }

    /// N5-6 — three players, two empty seats below P0's accept latch.
    #[test]
    fn matched_seats_without_tokens_each_keep_their_continuation_after_a_pause() {
        let outcome = run(Row::new(3, Board::HalvingP0)
            .optional()
            .rider(Rider::WhoDoesnt));
        assert_eq!(outcome.prompts, vec![(P0, "optional")]);
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![0, 1, 1]);
    }

    /// N5-7 — the empty seat first, before P1's pause.
    #[test]
    fn matched_first_seat_without_a_token_keeps_its_continuation_before_a_pause() {
        let outcome = run(Row::new(2, Board::HalvingP1)
            .optional()
            .rider(Rider::WhoDoesnt));
        assert_eq!(outcome.prompts, vec![(P1, "optional")]);
        assert_eq!(outcome.fractals, vec![(P1, 1)]);
        assert_eq!(outcome.life, vec![1, 0]);
    }

    /// N5-8 — no empty seat: every seat placed, so no decline rider fires.
    #[test]
    fn matched_seats_that_place_do_not_take_the_decline_rider() {
        let outcome = run(Row::new(2, Board::Empty).rider(Rider::WhoDoesnt));
        assert_eq!(outcome.fractals, vec![(P0, 1), (P1, 1)]);
        assert_eq!(outcome.life, vec![0, 0]);
    }

    /// N5-9 — "If it's your turn, [you may] put a counter on it. Otherwise,
    /// gain 2 life. You gain 3 life." P1 (empty, not its turn) takes its
    /// "otherwise" leg, and the trailing instruction still runs once.
    #[test]
    fn matched_empty_seat_otherwise_leg_keeps_the_trailing_instruction_after_a_pause() {
        let outcome = run(Row::new(2, Board::HalvingP0)
            .optional()
            .gate(Gate::YourTurn)
            .otherwise()
            .tail(SubAbilityLink::SequentialSibling));
        assert_eq!(outcome.prompts, vec![(P0, "optional")]);
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![3, 2]);
    }

    #[test]
    fn matched_empty_seat_otherwise_leg_keeps_the_trailing_instruction_in_apnap() {
        let outcome = run(Row::new(2, Board::HalvingP0)
            .gate(Gate::YourTurn)
            .otherwise()
            .tail(SubAbilityLink::SequentialSibling));
        assert_eq!(outcome.fractals, vec![(P0, 1)]);
        assert_eq!(outcome.life, vec![3, 2]);
    }

    /// N5-10 — two consecutive "otherwise" legs, each carrying what follows.
    #[test]
    fn matched_empty_seats_otherwise_legs_each_run_after_a_pause() {
        let row = Row::new(3, Board::HalvingP0)
            .gate(Gate::YourTurn)
            .otherwise()
            .tail(SubAbilityLink::SequentialSibling);
        let paused = run(row.optional());
        assert_eq!(paused.prompts, vec![(P0, "optional")]);
        assert_eq!(paused.life, vec![3, 2, 2]);
        let sync = run(row);
        assert_eq!(sync.life, vec![3, 2, 2]);
    }

    /// N5-11 — a failed condition with "each player who doesn't": the empty
    /// seat reads its own "not performed", not P0's accept latch.
    #[test]
    fn matched_empty_seat_failed_condition_reads_its_own_outcome_after_a_pause() {
        for link in LINKS {
            for (players, expected) in [(2, vec![3, 1]), (3, vec![3, 1, 1])] {
                let row = Row::new(players, Board::HalvingP0)
                    .gate(Gate::YourTurn)
                    .rider(Rider::WhoDoesnt)
                    .tail(link);
                let paused = run(row.optional());
                assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
                assert_eq!(paused.life, expected, "paused, {players}p, {link:?}");
                assert_eq!(run(row).life, expected, "sync, {players}p, {link:?}");
            }
        }
    }

    /// N5-12 — a failed condition governs the whole sentence, so the empty
    /// seat's dependent "and gain 1 life" is skipped; the trailing instruction
    /// still runs, whatever its link.
    #[test]
    fn matched_empty_seat_failed_condition_skips_its_dependent_continuation() {
        for link in LINKS {
            let row = Row::new(2, Board::HalvingP0)
                .optional()
                .gate(Gate::YourTurn)
                .rider(Rider::AndGain)
                .tail(link);
            let paused = run(row);
            assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
            assert_eq!(paused.life, vec![4, 0], "paused, 2p, {link:?}");
            let paused = run(Row { players: 3, ..row });
            assert_eq!(paused.life, vec![4, 0, 0], "paused, 3p, {link:?}");
            // Sync controls: on P1's (P2's) turn P0's condition fails, so no
            // seat is prompted; the empty seat whose condition holds is not
            // asserted (suppress-to-no-op limitation).
            let sync = run(row.active(P1));
            assert!(sync.prompts.is_empty());
            assert_eq!(sync.life[0], 3, "sync, 2p, {link:?}");
            let sync = run(Row { players: 3, ..row }.active(P2));
            assert!(sync.prompts.is_empty());
            assert_eq!(sync.life[..2], [3, 0], "sync, 3p, {link:?}");
        }
    }

    /// N5-14 — a failed condition with "if you do": the empty seat gains
    /// nothing, P0 (accepted) gains 1, and the trailing instruction runs.
    #[test]
    fn matched_empty_seat_failed_condition_false_if_you_do_keeps_the_trailing_instruction() {
        for link in LINKS {
            for (players, expected) in [(2, vec![4, 0]), (3, vec![4, 0, 0])] {
                let row = Row::new(players, Board::HalvingP0)
                    .gate(Gate::YourTurn)
                    .rider(Rider::IfYouDo)
                    .tail(link);
                let paused = run(row.optional());
                assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
                assert_eq!(paused.fractals, vec![(P0, 1)]);
                assert_eq!(paused.life, expected, "paused, {players}p, {link:?}");
                assert_eq!(run(row).life, expected, "sync, {players}p, {link:?}");
            }
        }
    }

    /// N5-15 — the unconditioned B2 shape: the empty seat's "if you do" is
    /// false, and the trailing instruction runs.
    #[test]
    fn matched_empty_seat_false_if_you_do_keeps_the_trailing_instruction() {
        for link in LINKS {
            for (players, board, prompted, expected) in [
                (2, Board::HalvingP0, P0, vec![4, 0]),
                (3, Board::HalvingP0, P0, vec![4, 0, 0]),
                (2, Board::HalvingP1, P1, vec![3, 1]),
            ] {
                let row = Row::new(players, board).rider(Rider::IfYouDo).tail(link);
                let paused = run(row.optional());
                assert_eq!(paused.prompts, vec![(prompted, "optional")], "{link:?}");
                assert_eq!(paused.fractals, vec![(prompted, 1)]);
                assert_eq!(paused.life, expected, "paused, {players}p, {link:?}");
                assert_eq!(run(row).life, expected, "sync, {players}p, {link:?}");
            }
        }
    }

    /// N5-16 — every seat `Bound`; the last seat does not place (its condition
    /// fails, or it declines its "may"). The trailing instruction runs once.
    #[test]
    fn bound_last_seat_that_does_not_place_keeps_the_trailing_instruction() {
        for link in LINKS {
            // (i) P0 Parallel Lives (P0 2 Fractals, P1 1); P1 controls no
            // enchantment, so its condition fails. P1's own life is not asserted.
            let row = Row::new(2, Board::ParallelP0)
                .gate(Gate::ControlsEnchantment)
                .rider(Rider::WhoDoesnt)
                .tail(link);
            let paused = run(row.optional());
            assert_eq!(paused.prompts, vec![(P0, "optional")], "{link:?}");
            assert_eq!(paused.fractals, vec![(P0, 1), (P0, 1), (P1, 0)]);
            assert_eq!(paused.life[0], 3, "paused (i), {link:?}");
            assert_eq!(run(row).life[0], 3, "sync (i), {link:?}");

            // (ii) unconditioned; P0 accepts, P1 declines.
            let row = Row::new(2, Board::Empty)
                .optional()
                .rider(Rider::IfYouDo)
                .tail(link);
            let declined = run(row.decline(P1));
            assert_eq!(
                declined.prompts,
                vec![(P0, "optional"), (P1, "optional")],
                "{link:?}"
            );
            assert_eq!(declined.life, vec![4, 0], "(ii), {link:?}");

            // (iii) three players; P1, then P2, declines.
            let row = Row { players: 3, ..row };
            assert_eq!(
                run(row.decline(P1)).life,
                vec![4, 0, 0],
                "(iii) P1, {link:?}"
            );
            assert_eq!(
                run(row.decline(P2)).life,
                vec![4, 1, 0],
                "(iii) P2, {link:?}"
            );
        }
    }

    /// L1 — CR 608.2c + CR 118.12a: "If it's your turn, put a counter on it
    /// unless [you] pay {1}. Each player who doesn't gains 1 life." The empty
    /// seat (P1, Halving Season) offers its unless payment once, and only when
    /// its condition holds: the condition is checked before the payment choice.
    #[test]
    fn matched_empty_seat_offers_its_unless_payment_once_behind_its_condition() {
        for (active, expected) in [(P1, vec![(P1, "unless")]), (P0, vec![(P0, "unless")])] {
            let row = Row::new(2, Board::HalvingP1)
                .gate(Gate::YourTurn)
                .rider(Rider::WhoDoesnt)
                .active(active);
            let outcome = settle(
                2,
                Board::HalvingP1,
                active,
                None,
                &|source| {
                    let mut chain = chain(source, &row);
                    chain
                        .sub_ability
                        .as_deref_mut()
                        .expect("the placement follows the creation")
                        .unless_pay = Some(UnlessPayModifier {
                        cost: AbilityCost::Mana {
                            cost: ManaCost::generic(1),
                        },
                        payer: TargetFilter::Controller,
                    });
                    chain
                },
                &mut |_| {},
            );
            assert_eq!(outcome.prompts, expected, "{active:?}'s turn");
        }
    }

    /// N5-13 — the empty seat's failed condition and false "if you do" before a
    /// placing seat: P2's leg is reached with its own ability.
    #[test]
    fn matched_empty_seat_before_a_placing_seat_after_a_pause() {
        for link in LINKS {
            let row = Row::new(3, Board::HalvingP2ParallelP0)
                .gate(Gate::ControlsEnchantment)
                .rider(Rider::IfYouDo)
                .tail(link);
            let paused = run(row.optional());
            assert_eq!(
                paused.prompts,
                vec![(P0, "replacement"), (P0, "optional"), (P2, "optional")],
                "{link:?}"
            );
            // P0 keeps one Fractal (Parallel Lives, then P2's Halving Season);
            // P0's counter is halved to 0 by P2's Halving Season (P0 is P2's
            // opponent); P1 created none; P2's own placement lands.
            assert_eq!(paused.fractals, vec![(P0, 0), (P2, 1)]);
            assert_eq!(paused.life[1..], [0, 1], "paused, {link:?}");
            let sync = run(row);
            assert!(sync.fractals.contains(&(P2, 1)));
            assert_eq!(sync.life[1..], [0, 1], "sync, {link:?}");
        }
    }
}
