//! Death by Dragons: "Each player other than target player creates a 5/5 red
//! Dragon creature token with flying." The exclusion anchor declares a
//! cast-time player target (CR 115.1a), so the spell fizzles when its only
//! target is illegal (CR 608.2b, ruling R1); otherwise every non-target player
//! creates exactly one Dragon and the targeted player creates none (ruling R2).
//!
//! Runtime rows drive the real cast pipeline with verbatim Oracle text.

use engine::game::filter_state_for_viewer;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ControllerRef, CopyTargetPurpose, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    GameState, PendingTokenSubstitute, ReplacementChoiceKind, TokenSubstituteChoice, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const DEATH_BY_DRAGONS: &str =
    "Each player other than target player creates a 5/5 red Dragon creature token with flying.";

const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.\nForetell {U} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";

fn game() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Grant the controller of `grantor` hexproof (Secret Rendezvous fizzle
/// recipe: the grant lives on a creature that player controls).
fn hexproof_for_controller(runner_state: &mut GameState, grantor: ObjectId) {
    let grant = vec![
        StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        )),
    ];
    let object = runner_state.objects.get_mut(&grantor).expect("grantor");
    object.base_static_definitions = std::sync::Arc::new(grant.clone());
    object.static_definitions = grant.into();
    runner_state.layers_dirty.mark_full();
    engine::game::layers::flush_layers(runner_state);
}

/// Every token on the battlefield controlled by `player`.
fn controlled_tokens(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.controller == player
        })
        .collect()
}

/// CR 111.3: the creating spell defines the token's Dragon subtype, 5/5 P/T,
/// red color, Creature type, and flying. The name check pins the engine's
/// subtype-named token convention; CR 111.4 would name it "Dragon Token".
fn assert_dragon(state: &GameState, id: ObjectId, controller: PlayerId) {
    let token = &state.objects[&id];
    assert_eq!(token.controller, controller);
    assert_eq!(token.name, "Dragon");
    assert_eq!((token.power, token.toughness), (Some(5), Some(5)));
    assert_eq!(token.color, vec![ManaColor::Red]);
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Dragon"),
        "token must retain its Dragon subtype: {:?}",
        token.card_types.subtypes
    );
    assert!(token.keywords.contains(&Keyword::Flying));
}

/// CR 608.2c + R2: with P1 (then P2, a non-consecutive seat) targeted in a
/// 3-player game, each non-target player creates exactly one Dragon and the
/// targeted player creates none.
#[test]
fn death_by_dragons_each_non_target_player_creates_one_dragon() {
    for target in [P1, P2] {
        let mut scenario = game();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).target_player(target).resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "target {target:?}: resolution must complete to Priority, got {:?}",
            outcome.final_waiting_for()
        );
        assert!(
            outcome.state().stack.is_empty(),
            "target {target:?}: stack must be empty after resolution"
        );
        for player in [P0, P1, P2] {
            let tokens = controlled_tokens(outcome.state(), player);
            if player == target {
                assert!(
                    tokens.is_empty(),
                    "target {target:?}: the targeted player creates no Dragons, got {}",
                    tokens.len()
                );
            } else {
                assert_eq!(
                    tokens.len(),
                    1,
                    "target {target:?}: {player:?} must create exactly one Dragon"
                );
                assert_dragon(outcome.state(), tokens[0], player);
            }
        }
    }
}

/// CR 608.2f + CR 614.5 (F1): Mystic Reflection's one-shot copy replacement
/// applies once to the whole simultaneous entry, so every non-target seat's
/// Dragon enters as a copy of the chosen creature and the target creates none.
#[test]
fn death_by_dragons_mystic_reflection_converts_every_seat() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(bears).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
    let outcome = runner.cast(dragons).target_player(P2).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must complete to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "stack must be empty after resolution"
    );
    for player in [P0, P1] {
        let tokens = controlled_tokens(outcome.state(), player);
        assert_eq!(tokens.len(), 1, "{player:?} must create exactly one token");
        let token = &outcome.state().objects[&tokens[0]];
        assert_eq!(
            token.name, "Grizzly Bears",
            "{player:?}'s token must enter as a copy of Grizzly Bears"
        );
        assert_eq!(
            (token.power, token.toughness),
            (Some(2), Some(2)),
            "{player:?}'s token must be 2/2"
        );
    }
    assert!(
        controlled_tokens(outcome.state(), P2).is_empty(),
        "the targeted player creates no tokens"
    );
    let dragons_left: Vec<_> = outcome
        .state()
        .battlefield
        .iter()
        .filter(|id| outcome.state().objects[id].name == "Dragon")
        .collect();
    assert!(
        dragons_left.is_empty(),
        "no Dragon tokens should remain once Mystic Reflection converts the entry"
    );
}

/// CR 608.2b + R1: the exclusion anchor declares the spell's only target, so
/// when the target is illegal as the spell resolves, the spell doesn't
/// resolve and no player creates a token.
///
/// Paired with the test above: that test proves the same spell resolves WITH
/// tokens when legal, so these zero-token assertions cannot pass vacuously on
/// a spell that never resolves.
#[test]
fn death_by_dragons_illegal_target_fizzles_with_no_tokens() {
    let mut scenario = game();
    let grantor = scenario.add_creature(P1, "Hexproof Grantor", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    let mut commit = runner.cast(spell).target_player(P1).commit();
    hexproof_for_controller(commit.state_mut(), grantor);
    assert!(
        engine::game::static_abilities::player_has_hexproof(commit.state(), P1),
        "reach: the target is illegal before resolution"
    );
    let outcome = commit.resolve();
    for player in [P0, P1, P2] {
        assert!(
            controlled_tokens(outcome.state(), player).is_empty(),
            "{player:?} must create no Dragons when the spell fizzles"
        );
    }
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must complete to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "stack must be empty after the fizzle"
    );
    outcome.assert_zone(&[spell], Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// F1/F2 simultaneous-token-clause matrix: one compound CreateToken through the
// replacement pipeline once (CR 608.2f fan-out, CR 614.5 whole-entry one-shot
// application), pausing for entry-copy choices (Reflection into Adaptive
// Automaton) and multi-replacement ordering (Doubling Season + Reflection).
// ---------------------------------------------------------------------------

/// Verbatim Oracle text (giada_metallic_mimic.rs): "As this creature enters,
/// choose a creature type. This creature is the chosen type in addition to its
/// other types. Other creatures you control of the chosen type get +1/+1."
const ADAPTIVE_AUTOMATON: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nOther creatures you control of the chosen type get +1/+1.";

/// Verbatim Oracle text (Scryfall unreachable in this environment; matches the
/// established in-repo const): token-doubling half plus counters half.
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";

const JINNIE_FAY: &str = "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.";

// Verbatim Oracle text (MTGJSON).
const ESIX: &str = "Flying\nThe first time you would create one or more tokens during each of your turns, you may instead choose a creature other than Esix and create that many tokens that are copies of that creature.";

// Verbatim Oracle text (MTGJSON).
const MOONLIT_MEDITATION: &str = "Enchant artifact or creature you control\nThe first time you would create one or more tokens each turn, you may instead create that many tokens that are copies of enchanted permanent.";

// Verbatim Oracle text (MTGJSON).
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// The engine's creature-type options come from `all_creature_types`; pin the
/// same list the as-enters-choice tests use so answers are always offered.
const CREATURE_TYPES: [&str; 7] = [
    "Angel",
    "Shapeshifter",
    "Elf",
    "Phyrexian",
    "Wizard",
    "Beast",
    "Wall",
];

fn set_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = CREATURE_TYPES.iter().map(|t| t.to_string()).collect();
}

fn automaton_on_battlefield(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, name, 2, 2, ADAPTIVE_AUTOMATON)
        .as_artifact_creature()
        .with_subtypes(vec!["Automaton"])
        .id()
}

/// A vanilla 2/2 carrying a real subtype, so a copy's anthem has a live reader.
fn typed_sibling(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    subtype: &str,
) -> ObjectId {
    scenario
        .add_creature(player, name, 2, 2)
        .with_subtypes(vec![subtype])
        .id()
}

fn season_in_hand(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Doubling Season", false, DOUBLING_SEASON)
        .as_enchantment()
        .id()
}

/// Cast a staged permanent spell and prove it resolved onto the battlefield
/// (reach guard for every replacement the test relies on).
fn cast_permanent(runner: &mut GameRunner, spell: ObjectId, name: &str) {
    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{name} must resolve to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "{name} must leave an empty stack"
    );
    assert_eq!(
        outcome.state().objects[&spell].zone,
        Zone::Battlefield,
        "{name} must be on the battlefield"
    );
}

/// Assert the live prompt is a `NamedChoice`, prove `choice` is offered, answer
/// it, and return the acting player.
fn answer_named_choice(runner: &mut GameRunner, choice: &str) -> PlayerId {
    let WaitingFor::NamedChoice {
        player,
        options,
        choice_type,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a NamedChoice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        options.contains(&choice.to_string()),
        "choice {choice:?} must be offered (choice_type={choice_type:?}), options={options:?}"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: choice.to_string(),
        })
        .expect("answer the named choice");
    player
}

/// Assert the live prompt is a CR 616.1 ordering choice asked of `actor` over
/// exactly the candidates sourced from `offered` (as a set), and answer it so
/// the candidate sourced from `first_source` applies first.
fn answer_ordering_putting_first(
    runner: &mut GameRunner,
    actor: PlayerId,
    offered: &[ObjectId],
    first_source: ObjectId,
) {
    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a ReplacementChoice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        kind,
        ReplacementChoiceKind::Order,
        "expected an ordering choice, got {kind:?}"
    );
    assert_eq!(
        player, actor,
        "the ordering choice belongs to {actor:?}, got {player:?}"
    );
    let mut offered_sources: Vec<ObjectId> = candidates
        .iter()
        .map(|candidate| candidate.source_id)
        .collect();
    offered_sources.sort_by_key(|id| id.0);
    let mut expected_sources = offered.to_vec();
    expected_sources.sort_by_key(|id| id.0);
    assert_eq!(
        offered_sources, expected_sources,
        "{actor:?} must be offered exactly its own candidates"
    );
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == first_source)
        .unwrap_or_else(|| {
            panic!("source {first_source:?} must be among the ordering candidates: {candidates:?}")
        });
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the ordering choice");
}

/// Accept index of an optional replacement prompt (index 1 declines).
const ACCEPT: usize = 0;
/// Decline index of an optional replacement prompt.
const DECLINE: usize = 1;

/// Assert the live prompt is an accept/decline choice for the replacement
/// sourced from `source`, asked of `actor`, and answer it with `index`.
fn answer_optional(runner: &mut GameRunner, actor: PlayerId, source: ObjectId, index: usize) {
    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected an optional ReplacementChoice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        kind,
        ReplacementChoiceKind::OptionalBranch,
        "expected an accept/decline choice, got {kind:?}"
    );
    assert_eq!(
        player, actor,
        "the accept/decline choice belongs to {actor:?}, got {player:?}"
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.source_id == source),
        "the optional prompt must offer {source:?}'s replacement: {candidates:?}"
    );
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the optional replacement");
}

/// Cast Mystic Reflection on `chosen` and prove it resolved.
fn resolve_reflection(runner: &mut GameRunner, mystic: ObjectId, chosen: ObjectId) {
    runner.cast(mystic).target_object(chosen).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
}

/// The completion boundary of a paused clause: Priority, an empty stack and
/// Death by Dragons in the graveyard, and the clause record released (CR
/// 608.2f: the paused clause finished).
fn assert_clause_completed(runner: &GameRunner, dragons: ObjectId) {
    assert_quiet_completion(runner, "the token clause");
    assert_eq!(
        runner.state().objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
    assert!(
        runner.state().pending_simultaneous_token_clause.is_none(),
        "the paused token clause must be finished and its record taken"
    );
}

/// Mystic Reflection's floating one-shot (the only floating shield in these
/// setups) and whether it has been used.
fn reflection_consumed(state: &GameState) -> bool {
    assert_eq!(
        state.pending_damage_replacements.len(),
        1,
        "reach guard: only Mystic Reflection's shield should be floating"
    );
    state.pending_damage_replacements[0].is_consumed
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|id| id.0);
    ids
}

fn assert_bears(state: &GameState, id: ObjectId, controller: PlayerId) {
    let token = &state.objects[&id];
    assert_eq!(token.controller, controller);
    assert_eq!(
        token.name, "Grizzly Bears",
        "{controller:?}'s token must be a Bear"
    );
    assert_eq!((token.power, token.toughness), (Some(2), Some(2)));
}

fn assert_angel(state: &GameState, id: ObjectId, controller: PlayerId) {
    let token = &state.objects[&id];
    assert_eq!(token.controller, controller);
    assert_eq!(token.name, "Angel");
    assert_eq!((token.power, token.toughness), (Some(4), Some(4)));
    assert_eq!(token.color, vec![ManaColor::White]);
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Angel"),
        "token must retain its Angel subtype: {:?}",
        token.card_types.subtypes
    );
    assert!(token.keywords.contains(&Keyword::Flying));
    assert!(token.keywords.contains(&Keyword::Vigilance));
}

fn assert_automaton_copy(state: &GameState, id: ObjectId, controller: PlayerId, chosen: &str) {
    let token = &state.objects[&id];
    assert_eq!(token.controller, controller);
    assert_eq!(token.name, "Adaptive Automaton");
    assert_eq!(
        token.chosen_creature_type(),
        Some(chosen),
        "the copy must remember its own choice"
    );
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == chosen),
        "the copy must carry its chosen type, subtypes={:?}",
        token.card_types.subtypes
    );
}

fn assert_no_dragons(state: &GameState) {
    let dragons: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[*id].name == "Dragon")
        .collect();
    assert!(
        dragons.is_empty(),
        "no Dragon tokens should remain, found {dragons:?}"
    );
}

fn last_created_sorted(state: &GameState) -> Vec<ObjectId> {
    let mut ids = state.last_created_token_ids.clone();
    ids.sort_by_key(|id| id.0);
    ids
}

fn assert_quiet_completion(runner: &GameRunner, what: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{what} must complete to Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "{what} must leave an empty stack"
    );
}

/// CR 608.2f + CR 614.5 (F1.1): Mystic Reflection converts the whole
/// simultaneous entry into Adaptive Automaton copies, and each seat's copy
/// asks its own controller for its creature type. Pre-fix this hits the
/// `mod.rs:25405` debug_assert (delivery paused on a gated shape).
#[test]
fn death_by_dragons_reflection_into_automaton_asks_each_seat_independently() {
    let mut scenario = game();
    let automaton = automaton_on_battlefield(&mut scenario, P0, "Adaptive Automaton");
    let elf = typed_sibling(&mut scenario, P0, "Elven Lookout", "Elf");
    let wizard = typed_sibling(&mut scenario, P1, "Wizard Mentor", "Wizard");
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    runner.cast(mystic).target_object(automaton).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );

    let _ = runner.cast(dragons).target_player(P2).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "the compound delivery must pause on P0's as-enters choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_simultaneous_token_clause.is_some(),
        "the pause-carrying clause record must be installed at the B0 park"
    );

    let first = answer_named_choice(&mut runner, "Elf");
    assert_eq!(first, P0, "the first choice belongs to P0's copy");
    // Hostile mid-state: the second choice is still open, so the deferred
    // publication must not have fired.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { player, .. } if player == P1
        ),
        "P1's prompt must be live after P0 answers, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_simultaneous_token_clause.is_some(),
        "S2 must not fire while P1's choice is still open"
    );
    assert_eq!(
        controlled_tokens(runner.state(), P0).len(),
        1,
        "P0's copy must already be on the battlefield mid-clause"
    );
    assert!(
        controlled_tokens(runner.state(), P1).is_empty(),
        "P1 must have no token before answering"
    );

    let second = answer_named_choice(&mut runner, "Wizard");
    assert_eq!(second, P1, "the second choice belongs to P1's copy");
    assert_quiet_completion(&runner, "the token clause");
    assert!(
        runner.state().pending_simultaneous_token_clause.is_none(),
        "the paused token clause must be finished and its record taken"
    );

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(
        p0_tokens.len(),
        1,
        "P0 must create exactly one Automaton copy"
    );
    assert_automaton_copy(state, p0_tokens[0], P0, "Elf");
    assert_eq!(
        (
            state.objects[&p0_tokens[0]].power,
            state.objects[&p0_tokens[0]].toughness
        ),
        (Some(2), Some(2))
    );
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(
        p1_tokens.len(),
        1,
        "P1 must create exactly one Automaton copy"
    );
    assert_automaton_copy(state, p1_tokens[0], P1, "Wizard");
    assert_eq!(
        (
            state.objects[&p1_tokens[0]].power,
            state.objects[&p1_tokens[0]].toughness
        ),
        (Some(2), Some(2))
    );
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert_eq!(
        (state.objects[&elf].power, state.objects[&elf].toughness),
        (Some(3), Some(3)),
        "P0's Elf sibling proves the copy's anthem is live"
    );
    assert_eq!(
        (
            state.objects[&wizard].power,
            state.objects[&wizard].toughness
        ),
        (Some(3), Some(3)),
        "P1's Wizard sibling proves the copy's anthem is live"
    );
    assert_eq!(
        state.objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
    let mut expected = vec![p0_tokens[0], p1_tokens[0]];
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        last_created_sorted(state),
        expected,
        "the created referent must be exactly both seats' tokens"
    );
}

/// CR 608.2f + CR 614.5 (F1.2): the clause-entry/exit feeds capture each
/// seat's tokens as its entry commits, and the deferred publication publishes
/// the exact union only at completion.
#[test]
fn death_by_dragons_reflection_clause_publishes_both_seats_tokens_at_completion() {
    let mut scenario = game();
    let automaton = automaton_on_battlefield(&mut scenario, P0, "Adaptive Automaton");
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    runner.cast(mystic).target_object(automaton).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );

    let _ = runner.cast(dragons).target_player(P2).resolve();
    let first = answer_named_choice(&mut runner, "Elf");
    assert_eq!(first, P0);
    // Mid-clause: P0's copy is fully on the battlefield with its type, P1 has
    // nothing yet, and P1's prompt is pending.
    let p0_mid = controlled_tokens(runner.state(), P0);
    assert_eq!(p0_mid.len(), 1, "P0's copy must exist mid-clause");
    assert_automaton_copy(runner.state(), p0_mid[0], P0, "Elf");
    assert!(
        controlled_tokens(runner.state(), P1).is_empty(),
        "P1 must have no token before answering"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { player, .. } if player == P1
        ),
        "P1's prompt must be pending, got {:?}",
        runner.state().waiting_for
    );

    let second = answer_named_choice(&mut runner, "Elf");
    assert_eq!(second, P1);
    assert_quiet_completion(&runner, "the token clause");
    assert!(
        runner.state().pending_simultaneous_token_clause.is_none(),
        "the paused token clause must be finished and its record taken"
    );

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p0_tokens.len(), 1);
    assert_eq!(p1_tokens.len(), 1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    let mut expected = vec![p0_tokens[0], p1_tokens[0]];
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        last_created_sorted(state),
        expected,
        "last_created_token_ids must contain BOTH seats' tokens, exactly"
    );
    assert_eq!(
        state.objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
}

/// CR 608.2f + CR 614.5 (F1.3): seat generality — targeting P0 moves the
/// whole pausing compound to P1/P2 with per-seat attribution intact.
#[test]
fn death_by_dragons_reflection_clause_excludes_first_seat_when_targeted() {
    let mut scenario = game();
    let automaton = automaton_on_battlefield(&mut scenario, P1, "Adaptive Automaton");
    let elf = typed_sibling(&mut scenario, P1, "Elven Lookout", "Elf");
    let wizard = typed_sibling(&mut scenario, P2, "Wizard Mentor", "Wizard");
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    runner.cast(mystic).target_object(automaton).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );

    let _ = runner.cast(dragons).target_player(P0).resolve();
    let first = answer_named_choice(&mut runner, "Elf");
    assert_eq!(first, P1, "the first choice belongs to P1's copy");
    let second = answer_named_choice(&mut runner, "Wizard");
    assert_eq!(second, P2, "the second choice belongs to P2's copy");
    assert_quiet_completion(&runner, "the token clause");
    assert!(
        runner.state().pending_simultaneous_token_clause.is_none(),
        "the paused token clause must be finished and its record taken"
    );

    let state = runner.state();
    assert!(
        controlled_tokens(state, P0).is_empty(),
        "the targeted player creates no tokens"
    );
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1);
    assert_automaton_copy(state, p1_tokens[0], P1, "Elf");
    let p2_tokens = controlled_tokens(state, P2);
    assert_eq!(p2_tokens.len(), 1);
    assert_automaton_copy(state, p2_tokens[0], P2, "Wizard");
    assert_no_dragons(state);
    assert_eq!(
        (state.objects[&elf].power, state.objects[&elf].toughness),
        (Some(3), Some(3)),
        "P1's Elf sibling proves the copy's anthem is live"
    );
    assert_eq!(
        (
            state.objects[&wizard].power,
            state.objects[&wizard].toughness
        ),
        (Some(3), Some(3)),
        "P2's Wizard sibling proves the copy's anthem is live"
    );
    assert_eq!(
        state.objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
}

/// CR 608.2f + CR 614.5 + CR 616.1g (F2.1): Doubling Season plus Mystic
/// Reflection on the compound event. Reflection modifies the entry contained
/// in the creation, so it can't be chosen until Season has applied
/// (CR 616.1g; Reflection ruling: "control-changing effects ..., then copy
/// effects ..., then all other effects"). No ordering prompt is asked: P0 gets
/// two Bears (doubled, then converted) and P1 gets one. Hardening: green at
/// base by design; it pins that no prompt is asked at all.
#[test]
fn death_by_dragons_season_and_reflection_apply_per_batch_without_prompt() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let season = season_in_hand(&mut scenario, P0);
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);
    cast_permanent(&mut runner, season, "Doubling Season");

    let _ = runner.cast(dragons).target_player(P2).resolve();
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(
        p0_tokens.len(),
        2,
        "P0 must create two Bears (doubled then converted)"
    );
    for id in &p0_tokens {
        assert_bears(state, *id, P0);
    }
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one Bear");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p0_tokens[1], p1_tokens[0]]),
        "the created referent must be all three Bears"
    );
}

/// CR 608.2f + CR 614.5 + CR 616.1 (F1): a declined Jinnie Fay keeps one
/// shared entry. P0 alone decides Jinnie (it applies only to P0's tokens);
/// after the decline, Reflection converts every seat's Dragon at once
/// (rulings: "All the tokens enter at the same time."). Pre-fix the optional
/// kept the sequential legs, so Reflection was used on P0's leg and P1 kept a
/// Dragon.
#[test]
fn death_by_dragons_declined_optional_keeps_one_shared_entry() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie = scenario
        .add_creature_to_hand_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    // Jinnie must enter BEFORE Reflection resolves: her own entry is a
    // creature entering, which would consume the one-shot (CR 614.5).
    cast_permanent(&mut runner, jinnie, "Jinnie Fay");
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, jinnie, DECLINE);
    // No ordering prompt follows: Reflection is withheld until the creation's
    // replacements are chosen (CR 616.1g), then applies alone.
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// CR 608.2f + CR 614.5 + CR 616.1 (F1): accepting Jinnie Fay completes in
/// one clause. The accept decision and the Cat/Dog choice are P0's (Jinnie
/// applies only to P0's tokens; its chooser is its controller), and P1's
/// Dragon is converted by Reflection.
///
/// CR 614.6 + CR 608.2f + CR 616.1g: the accepted substitute rewrites P0's
/// part of the one simultaneous event before the shared entry, so Reflection
/// converts it too and P0's token enters as a Bear (The Pit ruling: every
/// token enters at the same time; the Jinnie ruling keeps the creating
/// instruction's entry state).
/// The paused clause record is server authority (it can hold hidden-zone moves, CR 400.2 + CR 800.4a), absent from every viewer projection.
#[test]
fn death_by_dragons_accepted_optional_completes_one_clause() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie = scenario
        .add_creature_to_hand_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    cast_permanent(&mut runner, jinnie, "Jinnie Fay");
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, jinnie, ACCEPT);
    let WaitingFor::ChooseOneOfBranch { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "accepting Jinnie Fay must ask for Cat or Dog, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Jinnie's controller chooses Cat or Dog");
    assert!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .is_some_and(|pending| {
                matches!(
                    pending.token_substitute,
                    Some(PendingTokenSubstitute {
                        awaits: TokenSubstituteChoice::Branch,
                        ..
                    })
                )
            }),
        "the accept parks the branch substitute on the compound, got {:?}",
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.token_substitute)
    );
    assert!(
        controlled_tokens(runner.state(), P0).is_empty()
            && controlled_tokens(runner.state(), P1).is_empty(),
        "nothing is delivered before the substitute choice"
    );
    assert!(
        runner.state().pending_simultaneous_token_clause.is_some(),
        "reach guard: the paused clause record is installed (still unfed: \
         nothing is delivered before the substitute choice)"
    );
    for viewer in [P0, P1, P2] {
        assert!(
            filter_state_for_viewer(runner.state(), viewer)
                .pending_simultaneous_token_clause
                .is_none(),
            "viewer {viewer:?} must not receive the paused clause record"
        );
    }
    assert!(
        runner.state().pending_simultaneous_token_clause.is_some(),
        "filtering must not alter the authoritative record"
    );
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose the Cat branch");
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create exactly one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// CR 608.2f + CR 614.5: accepting Jinnie Fay finishes the paused clause at the priority boundary, so the next simultaneous token clause still enters as one event.
#[test]
fn death_by_dragons_accepted_optional_releases_the_clause_for_the_next_fan_out() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie = scenario
        .add_creature_to_hand_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let mystic_1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons_1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mystic_2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons_2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    cast_permanent(&mut runner, jinnie, "Jinnie Fay");
    resolve_reflection(&mut runner, mystic_1, bears);

    // First clause, exactly as the accepted-optional row: accept, then Cat.
    let _ = runner.cast(dragons_1).target_player(P2).resolve();
    answer_optional(&mut runner, P0, jinnie, ACCEPT);
    let WaitingFor::ChooseOneOfBranch { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "accepting Jinnie Fay must ask for Cat or Dog, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Jinnie's controller chooses Cat or Dog");
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose the Cat branch");
    assert_clause_completed(&runner, dragons_1);
    let p0_before = controlled_tokens(runner.state(), P0);
    let p1_before = controlled_tokens(runner.state(), P1);
    assert_eq!(
        p0_before.len(),
        1,
        "the first clause creates P0's token exactly once"
    );
    assert_bears(runner.state(), p0_before[0], P0);

    // Second clause: a fresh Reflection, then a second Death by Dragons.
    resolve_reflection(&mut runner, mystic_2, bears);
    let _ = runner.cast(dragons_2).target_player(P2).resolve();
    answer_optional(&mut runner, P0, jinnie, DECLINE);
    assert_clause_completed(&runner, dragons_2);

    let state = runner.state();
    let p0_new: Vec<ObjectId> = controlled_tokens(state, P0)
        .into_iter()
        .filter(|id| !p0_before.contains(id))
        .collect();
    let p1_new: Vec<ObjectId> = controlled_tokens(state, P1)
        .into_iter()
        .filter(|id| !p1_before.contains(id))
        .collect();
    assert_eq!(p0_new.len(), 1, "P0 must create exactly one new token");
    assert_bears(state, p0_new[0], P0);
    assert_eq!(p1_new.len(), 1, "P1 must create exactly one new token");
    assert_bears(state, p1_new[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert_eq!(
        state.pending_damage_replacements.len(),
        2,
        "reach guard: both Mystic Reflections' shields should be floating"
    );
    assert!(
        state
            .pending_damage_replacements
            .iter()
            .all(|shield| shield.is_consumed),
        "each Reflection must be used by its own shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_new[0], p1_new[0]]),
        "the second created referent must be both new Bears"
    );
}

/// CR 616.1 + CR 101.4: two Jinnie Fays, one per seat. Each applies only to
/// its controller's tokens, so each owner decides its own in APNAP order (P0,
/// then P1), and then Reflection converts the one shared entry.
#[test]
fn death_by_dragons_two_optional_owners_decide_in_apnap_order() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie_p0 = scenario
        .add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let jinnie_p1 = scenario
        .add_creature_from_oracle(P1, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, jinnie_p0, DECLINE);
    answer_optional(&mut runner, P1, jinnie_p1, DECLINE);
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// CR 614.6 + CR 608.2f + CR 616.1g (Esix R4: "This effect applies before
/// anything that modifies how those tokens enter the battlefield"): Esix's
/// creature choice happens inside the pipeline, and the copies enter with the
/// shared entry, so Reflection converts them too.
#[test]
fn death_by_dragons_accepted_esix_copy_enters_with_the_shared_entry() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let esix = scenario
        .add_creature_from_oracle(P0, "Esix, Fractal Bloom", 6, 6, ESIX)
        .id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, esix, ACCEPT);
    let WaitingFor::CopyTargetChoice {
        player,
        valid_targets,
        purpose,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "accepting Esix must ask for the creature to copy, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Esix's controller chooses the copy source");
    assert_eq!(
        purpose,
        CopyTargetPurpose::CopyTokenSource,
        "the prompt is the copy-source choice"
    );
    assert!(
        valid_targets.contains(&giant) && valid_targets.contains(&bears),
        "Hill Giant and Bears are legal copy sources, got {valid_targets:?}"
    );
    assert!(
        !valid_targets.contains(&esix),
        "Esix excludes itself, got {valid_targets:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(giant)),
        })
        .expect("choose the Hill Giant to copy");
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create exactly one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// Decline control for the Esix row: declining keeps the shared entry — both
/// seats' Dragons enter as Bears.
#[test]
fn death_by_dragons_declined_esix_keeps_the_shared_entry() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let esix = scenario
        .add_creature_from_oracle(P0, "Esix, Fractal Bloom", 6, 6, ESIX)
        .id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);
    let _ = giant;

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, esix, DECLINE);
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
}

/// CR 614.6 + CR 608.2f + CR 616.1g (Moonlit R4: "The effect of Moonlit
/// Meditation's last ability applies before anything that modifies how those
/// tokens enter the battlefield"): Moonlit asks nothing after the accept — the
/// copy rewrites P0's part at once and enters with the shared entry.
#[test]
fn death_by_dragons_accepted_moonlit_copy_enters_with_the_shared_entry() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let moonlit = scenario
        .add_enchantment_from_oracle(P0, "Moonlit Meditation", MOONLIT_MEDITATION)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    runner.attach_as_bestowed_aura(moonlit, giant);
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, moonlit, ACCEPT);
    // No further prompt: the choice-free substitute rewrote P0's part at once.
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create exactly one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// CR 614.6 + CR 608.2f + CR 616.1g + CR 101.4: two substitute deciders on one
/// compound decide in APNAP order, and both copies enter in the one shared
/// entry Reflection converts.
#[test]
fn death_by_dragons_two_accepted_substitutes_decide_in_apnap_order() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie_p0 = scenario
        .add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let jinnie_p1 = scenario
        .add_creature_from_oracle(P1, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P0, jinnie_p0, ACCEPT);
    let WaitingFor::ChooseOneOfBranch { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "P0's accept must park on P0's branch choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "P1 is offered nothing before P0's branch is answered (APNAP)"
    );
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("P0 chooses the Cat branch");
    answer_optional(&mut runner, P1, jinnie_p1, ACCEPT);
    let WaitingFor::ChooseOneOfBranch { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "P1's accept must park on P1's branch choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "the fourth prompt is P1's branch choice");
    runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("P1 chooses the Dog branch");
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create exactly one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create exactly one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// The parked substitute is a parked replacement: when its decider leaves, the
/// prompt and the record are torn down like any other parked replacement. The
/// survivors' part is phase 3's work and is not asserted here.
#[test]
fn death_by_dragons_substitute_decider_concedes_is_torn_down() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie = scenario
        .add_creature_from_oracle(P1, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_optional(&mut runner, P1, jinnie, ACCEPT);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: _p1,
                ..
            } if _p1 == P1
        ),
        "the accept parks on P1's branch choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes while its substitute is parked");

    assert!(
        runner.state().players[1].is_eliminated,
        "reach guard: P1 left the game"
    );
    assert!(
        runner.state().pending_replacement.is_none(),
        "eliminating the parked decider clears the parked substitute"
    );
    assert!(
        runner.state().waiting_for.acting_player() != Some(P1),
        "no prompt waits on the departed decider, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_simultaneous_token_clause.is_none(),
        "the paused token clause is gone after the Concede boundary"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack is empty after the Concede boundary"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game is at Priority after the Concede boundary, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 616.1 + CR 614.5 (F2): P0's Halving Season and P1's Doubling Season both
/// apply only to P1's tokens, so P1 (the affected batch owner) orders them and
/// is offered only those two (Halving Season rulings: "apply the replacement
/// effects one at a time, rounding down each time"; CR 616.1: the affected
/// player chooses the order whoever controls the sources).
fn halving_doubling_ordered_by_affected_owner(halving_first: bool) {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let halving = scenario
        .add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON)
        .id();
    let doubling_p1 = scenario
        .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    let choices = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter(|action| matches!(action, GameAction::ChooseReplacement { .. }))
        .count();
    assert_eq!(choices, 2, "exactly P1's two candidates are answerable");
    let first = if halving_first { halving } else { doubling_p1 };
    answer_ordering_putting_first(&mut runner, P1, &[halving, doubling_p1], first);
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0's creation is untouched");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    let mut expected = vec![p0_tokens[0]];
    if halving_first {
        // 1 -> 0 (halved, rounded down) -> 0 (doubled).
        assert!(p1_tokens.is_empty(), "P1 must create no tokens");
    } else {
        // 1 -> 2 (doubled) -> 1 (halved).
        assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
        assert_bears(state, p1_tokens[0], P1);
        expected.push(p1_tokens[0]);
    }
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by P0's entering token"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(expected),
        "the created referent must be exactly the Bears created"
    );
}

#[test]
fn death_by_dragons_halving_first_ordered_by_affected_owner() {
    halving_doubling_ordered_by_affected_owner(true);
}

#[test]
fn death_by_dragons_doubling_first_ordered_by_affected_owner() {
    halving_doubling_ordered_by_affected_owner(false);
}

/// Which of Halving Season and the owner's Doubling Season an owner applies
/// first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum First {
    Halving,
    Doubling,
}

/// CR 616.1 + CR 614.5 + CR 101.4: each batch owner orders the shared creation
/// replacements for their own tokens, in APNAP order (Halving Season ruling:
/// one at a time, rounding down). P0's Halving Season applies to both P1's and
/// P2's tokens; P1 and P2 each control a Doubling Season.
fn shared_halving_orders_each_owner(p1_first: First, p2_first: First) {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let halving = scenario
        .add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON)
        .id();
    let doubling_p1 = scenario
        .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
        .id();
    let doubling_p2 = scenario
        .add_enchantment_from_oracle(P2, "Doubling Season", DOUBLING_SEASON)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P0).resolve();
    let source_of = |first: First, doubling: ObjectId| match first {
        First::Halving => halving,
        First::Doubling => doubling,
    };
    answer_ordering_putting_first(
        &mut runner,
        P1,
        &[halving, doubling_p1],
        source_of(p1_first, doubling_p1),
    );
    answer_ordering_putting_first(
        &mut runner,
        P2,
        &[halving, doubling_p2],
        source_of(p2_first, doubling_p2),
    );
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    assert!(
        controlled_tokens(state, P0).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_eq!(
        state.objects[&bears].zone,
        Zone::Battlefield,
        "reach guard: the copy target is still on the battlefield"
    );
    // Halving first: 1 -> 0 (halved, rounded down) -> 0 (doubled).
    // Doubling first: 1 -> 2 (doubled) -> 1 (halved).
    let mut created = Vec::new();
    for (player, first) in [(P1, p1_first), (P2, p2_first)] {
        let tokens = controlled_tokens(state, player);
        match first {
            First::Halving => assert!(
                tokens.is_empty(),
                "{player:?} applied Halving Season first and must create no tokens"
            ),
            First::Doubling => {
                assert_eq!(tokens.len(), 1, "{player:?} must create one token");
                assert_bears(state, tokens[0], player);
                created.extend(tokens);
            }
        }
    }
    assert_no_dragons(state);
    // CR 701.7a: Reflection is used only by a token that actually enters.
    assert_eq!(
        reflection_consumed(state),
        !created.is_empty(),
        "Reflection must be used exactly when a token enters"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(created),
        "the created referent must be exactly the Bears created"
    );
}

#[test]
fn death_by_dragons_shared_halving_first_leaves_the_next_owner_its_order() {
    shared_halving_orders_each_owner(First::Halving, First::Doubling);
}

#[test]
fn death_by_dragons_both_owners_halving_first_create_nothing() {
    shared_halving_orders_each_owner(First::Halving, First::Halving);
}

#[test]
fn death_by_dragons_shared_halving_asks_each_owner_in_apnap_order() {
    shared_halving_orders_each_owner(First::Doubling, First::Doubling);
}

// Verbatim Oracle text (MTGJSON).
const PRIMAL_VIGOR: &str = "If one or more tokens would be created, twice that many of those tokens are created instead.\nIf one or more +1/+1 counters would be put on a creature, twice that many +1/+1 counters are put on that creature instead.";

// Verbatim Oracle text (MTGJSON).
const CRAFTY_CUTPURSE: &str = "Flash\nWhen this creature enters, each token that would be created under an opponent's control this turn is created under your control instead.";

/// The floating one-shots (`consume_on_apply`) on the board, as their
/// `is_consumed` flags, for boards with more than one floating definition.
fn one_shots_consumed(state: &GameState) -> Vec<bool> {
    state
        .pending_damage_replacements
        .iter()
        .filter(|def| def.consume_on_apply)
        .map(|def| def.is_consumed)
        .collect()
}

/// Assert the live prompt is a CR 616.1 ordering choice asked of `actor` over
/// exactly the two floating Mystic Reflections (floating indices 0 and 1), and
/// answer it so the one at floating index `first` applies first. Floating
/// definitions all display the `ObjectId(0)` sentinel as their source, so the
/// offered candidates are read from the parked replacement record.
fn answer_floating_ordering_putting_first(runner: &mut GameRunner, actor: PlayerId, first: usize) {
    let WaitingFor::ReplacementChoice {
        player,
        candidates: summaries,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a ReplacementChoice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(kind, ReplacementChoiceKind::Order);
    assert_eq!(
        player, actor,
        "the ordering choice belongs to {actor:?}, got {player:?}"
    );
    let candidates = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("an ordering prompt parks its replacement record")
        .candidates
        .clone();
    assert_eq!(candidates.len(), summaries.len());
    assert!(
        candidates.iter().all(|rid| rid.source == ObjectId(0)),
        "{actor:?} must be offered only floating candidates: {candidates:?}"
    );
    let mut indices: Vec<usize> = candidates.iter().map(|rid| rid.index).collect();
    indices.sort_unstable();
    assert_eq!(
        indices,
        vec![0, 1],
        "{actor:?} must be offered exactly both Reflections"
    );
    let index = candidates
        .iter()
        .position(|rid| rid.index == first)
        .expect("the chosen Reflection is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the ordering choice");
}

fn assert_hill_giant(state: &GameState, id: ObjectId, controller: PlayerId) {
    let token = &state.objects[&id];
    assert_eq!(token.controller, controller);
    assert_eq!(
        token.name, "Hill Giant",
        "{controller:?}'s token must be a Hill Giant"
    );
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
}

/// CR 616.1 + CR 614.5 + CR 101.4: P0's Halving Season and Primal Vigor both
/// reach P1's and P2's tokens (Halving Season ruling: "won't change the number
/// of tokens you create"; Primal Vigor ruling: "It doesn't matter who controls
/// the tokens"). Each owner orders the same pair for their own tokens: P1 puts
/// Halving Season first (1 -> 0 -> 0), and P2 is still asked and puts Primal
/// Vigor first (1 -> 2 -> 1).
#[test]
fn death_by_dragons_each_owner_orders_shared_replacements() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let halving = scenario
        .add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON)
        .id();
    let vigor = scenario
        .add_enchantment_from_oracle(P0, "Primal Vigor", PRIMAL_VIGOR)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P0).resolve();
    answer_ordering_putting_first(&mut runner, P1, &[halving, vigor], halving);
    answer_ordering_putting_first(&mut runner, P2, &[halving, vigor], vigor);
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    assert!(
        controlled_tokens(state, P0).is_empty(),
        "the targeted player creates no tokens"
    );
    assert!(
        controlled_tokens(state, P1).is_empty(),
        "P1 put Halving Season first and must create no tokens"
    );
    let p2_tokens = controlled_tokens(state, P2);
    assert_eq!(p2_tokens.len(), 1, "P2 must create one token");
    assert_bears(state, p2_tokens[0], P2);
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by P2's entering token"
    );
    assert_eq!(
        last_created_sorted(state),
        vec![p2_tokens[0]],
        "the created referent must be exactly P2's Bear"
    );
}

/// CR 616.1b + CR 111.2 + CR 614.5: P0 and P1 each resolved Crafty Cutpurse.
/// Crafty Cutpurse ruling: the token's would-be controller chooses one of the
/// applicable Crafty Cutpurse effects, then the new would-be controller
/// repeats the process among the remaining ones; "Each effect can be applied
/// to the token only once this way." So each Dragon goes to the other player
/// and comes back: every batch keeps its own applied set through the owner
/// change, and its current owner chooses. No prompt is raised (each owner has
/// one applicable effect at a time).
#[test]
fn death_by_dragons_two_cutpurses_return_each_dragon_to_its_creator() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let cutpurse_p0 = scenario
        .add_creature_to_hand_from_oracle(P0, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let cutpurse_p1 = scenario
        .add_creature_to_hand_from_oracle(P1, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    runner.cast(cutpurse_p0).resolve();
    assert_quiet_completion(&runner, "P0's Crafty Cutpurse");
    // P0 passes so P1 holds priority and flashes in its own Cutpurse.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner.cast(cutpurse_p1).resolve();
    assert_quiet_completion(&runner, "P1's Crafty Cutpurse");
    resolve_reflection(&mut runner, mystic, bears);

    // Setup reach guard: one unconsumed redirect per controller.
    for player in [P0, P1] {
        let redirects: Vec<bool> = runner
            .state()
            .pending_damage_replacements
            .iter()
            .filter(|def| {
                def.token_owner_redirect.is_some() && def.source_controller == Some(player)
            })
            .map(|def| def.is_consumed)
            .collect();
        assert_eq!(
            redirects,
            vec![false],
            "reach guard: {player:?}'s Cutpurse redirect is installed and unused"
        );
    }
    assert_eq!(
        runner.state().waiting_for.acting_player(),
        Some(P0),
        "reach guard: P0 holds priority to cast Death by Dragons"
    );

    let _ = runner.cast(dragons).target_player(P2).resolve();
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0's Dragon must come back to P0");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1's Dragon must come back to P1");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert_eq!(
        one_shots_consumed(state),
        vec![true],
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// Pass budget: with seven players, Primal Vigor and six Doubling Seasons (one
/// per creating seat) and Mystic Reflection, each owner applies every
/// replacement that reaches their own tokens. Each seat: 1 x 2 (Primal Vigor)
/// x 2 (its Doubling Season) = 4 tokens, each entering as a copy of Grizzly
/// Bears (CR 608.2f: "they enter as copies"). No prompt: Primal Vigor and
/// Doubling Season commute, and one Reflection is a mandatory singleton.
#[test]
fn death_by_dragons_six_owners_each_get_every_replacement() {
    let mut scenario = GameScenario::new_n_player(7, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.add_enchantment_from_oracle(P0, "Primal Vigor", PRIMAL_VIGOR);
    let seats: Vec<PlayerId> = (1..=6).map(PlayerId).collect();
    for seat in &seats {
        scenario.add_enchantment_from_oracle(*seat, "Doubling Season", DOUBLING_SEASON);
    }
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P0).resolve();
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    assert!(
        controlled_tokens(state, P0).is_empty(),
        "the targeted player creates no tokens"
    );
    let mut created = Vec::new();
    for seat in &seats {
        let tokens = controlled_tokens(state, *seat);
        assert_eq!(tokens.len(), 4, "{seat:?} must create four tokens");
        for id in &tokens {
            assert_bears(state, *id, *seat);
        }
        created.extend(tokens);
    }
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(created),
        "the created referent must be all 24 Bears"
    );
}

/// CR 616.1c + CR 616.1f + CR 614.5: two Mystic Reflections (A on Grizzly
/// Bears, B on Hill Giant). Mystic Reflection ruling: the controller of each
/// entering permanent chooses the order of the copy effects, and "The copy
/// effect they apply last will determine which creature the entering
/// permanent copies." P0 puts A first, so P0's token copies Hill Giant; P1 is
/// still asked and puts B first, so P1's token copies Grizzly Bears. Each
/// one-shot is spent by the event (CR 608.2f), not by P0's application.
#[test]
fn death_by_dragons_each_owner_orders_two_reflections() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mystic_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let mystic_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    // Each Reflection installs one floating definition in cast order, so A is
    // floating index 0 and B is floating index 1.
    resolve_reflection(&mut runner, mystic_a, bears);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    resolve_reflection(&mut runner, mystic_b, giant);
    assert_eq!(runner.state().pending_damage_replacements.len(), 2);
    let (reflection_a, reflection_b) = (0, 1);

    let _ = runner.cast(dragons).target_player(P2).resolve();
    answer_floating_ordering_putting_first(&mut runner, P0, reflection_a);
    answer_floating_ordering_putting_first(&mut runner, P1, reflection_b);
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create one token");
    assert_hill_giant(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    assert_eq!(
        state.pending_damage_replacements.len(),
        2,
        "reach guard: exactly the two Reflections are floating"
    );
    assert_eq!(
        one_shots_consumed(state),
        vec![true, true],
        "both Reflections must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p0_tokens[0], p1_tokens[0]]),
        "the created referent must be both copies"
    );
}

/// CR 101.4 + CR 616.1: an earlier APNAP owner's lone optional is
/// decided before a later owner orders. P1's Jinnie Fay applies only to P1's
/// tokens; P1's Halving Season applies only to P2's (ruling: "won't change the
/// number of tokens you create"), as does P2's Doubling Season. P1 decides
/// Jinnie first, then P2 orders its two candidates (rulings of The Pit and
/// Liege of the Hollows: choices in turn order, then one simultaneous entry).
#[test]
fn death_by_dragons_earlier_owner_optional_precedes_later_ordering() {
    let mut scenario = game();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jinnie_p1 = scenario
        .add_creature_from_oracle(P1, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let halving = scenario
        .add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON)
        .id();
    let doubling_p2 = scenario
        .add_enchantment_from_oracle(P2, "Doubling Season", DOUBLING_SEASON)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    resolve_reflection(&mut runner, mystic, bears);

    let _ = runner.cast(dragons).target_player(P0).resolve();
    answer_optional(&mut runner, P1, jinnie_p1, DECLINE);
    answer_ordering_putting_first(&mut runner, P2, &[halving, doubling_p2], doubling_p2);
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    assert!(
        controlled_tokens(state, P0).is_empty(),
        "the targeted player creates no tokens"
    );
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one token");
    assert_bears(state, p1_tokens[0], P1);
    // 1 -> 2 (doubled) -> 1 (halved).
    let p2_tokens = controlled_tokens(state, P2);
    assert_eq!(p2_tokens.len(), 1, "P2 must create one token");
    assert_bears(state, p2_tokens[0], P2);
    assert_no_dragons(state);
    assert!(
        reflection_consumed(state),
        "Reflection must be used by the shared entry"
    );
    assert_eq!(
        last_created_sorted(state),
        sorted(vec![p1_tokens[0], p2_tokens[0]]),
        "the created referent must be both Bears"
    );
}

/// CR 109.5 + CR 111.2 + CR 608.2f: "If you would create" reaches only Jinnie's controller's tokens, so two opponents' simultaneous Dragons raise no prompt.
#[test]
fn death_by_dragons_opponents_tokens_are_not_offered_your_jinnie() {
    let mut scenario = game();
    let jinnie = scenario
        .add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 4, JINNIE_FAY)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&jinnie].zone,
        Zone::Battlefield,
        "reach guard: Jinnie Fay is on the battlefield"
    );

    let _ = runner.cast(dragons).target_player(P0).resolve();
    // No Jinnie prompt for either opponent's batch: the clause completes at once.
    assert_clause_completed(&runner, dragons);

    let state = runner.state();
    assert!(
        controlled_tokens(state, P0).is_empty(),
        "the targeted player creates no tokens"
    );
    for player in [P1, P2] {
        let tokens = controlled_tokens(state, player);
        assert_eq!(
            tokens.len(),
            1,
            "{player:?} must create exactly one Dragon, unreplaced by P0's Jinnie Fay"
        );
        assert_dragon(state, tokens[0], player);
    }
}

/// CR 608.2f + CR 614.5 + CR 616.1g (DUAL): Season plus Reflection into
/// Automaton — no ordering choice (Reflection is withheld until Season has
/// applied), then three per-copy type choices (two for P0's doubled batch,
/// one for P1's), each answered by its own controller.
#[test]
fn death_by_dragons_season_and_reflection_into_automaton_asks_three_choices() {
    let mut scenario = game();
    let automaton = automaton_on_battlefield(&mut scenario, P0, "Adaptive Automaton");
    let season = season_in_hand(&mut scenario, P0);
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    runner.cast(mystic).target_object(automaton).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
    cast_permanent(&mut runner, season, "Doubling Season");

    let _ = runner.cast(dragons).target_player(P2).resolve();
    let mut type_answers: Vec<PlayerId> = Vec::new();
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        match runner.state().waiting_for.clone() {
            // CR 616.1g: Reflection is withheld until Season has applied, so
            // no replacement prompt is asked at all.
            WaitingFor::ReplacementChoice { player, kind, .. } => {
                panic!("no replacement prompt is expected, got {kind:?} for {player:?}");
            }
            WaitingFor::NamedChoice { player, .. } => {
                let prior = type_answers.iter().filter(|seat| **seat == player).count();
                let choice = match (player, prior) {
                    (P0, 0) => "Elf",
                    (P0, 1) => "Wizard",
                    (P1, 0) => "Beast",
                    _ => panic!("unexpected extra type choice for {player:?}"),
                };
                let actor = answer_named_choice(&mut runner, choice);
                assert_eq!(actor, player);
                type_answers.push(player);
            }
            other => panic!("unexpected prompt on the compound path: {other:?}"),
        }
    }
    assert_eq!(
        type_answers.len(),
        3,
        "two type choices for P0's doubled batch plus one for P1's"
    );
    assert_quiet_completion(&runner, "the token clause");
    assert!(
        runner.state().pending_simultaneous_token_clause.is_none(),
        "the paused token clause must be finished and its record taken"
    );

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 2, "P0 must create two copies");
    let mut p0_chosen: Vec<&str> = p0_tokens
        .iter()
        .map(|id| {
            assert_automaton_copy(
                state,
                *id,
                P0,
                state.objects[id].chosen_creature_type().unwrap_or("<none>"),
            );
            state.objects[id].chosen_creature_type().unwrap_or("<none>")
        })
        .collect();
    p0_chosen.sort_unstable();
    assert_eq!(
        p0_chosen,
        vec!["Elf", "Wizard"],
        "P0's two copies must hold independent choices"
    );
    for id in &p0_tokens {
        assert_eq!(
            (state.objects[id].power, state.objects[id].toughness),
            (Some(2), Some(2))
        );
    }
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 1, "P1 must create one copy");
    assert_automaton_copy(state, p1_tokens[0], P1, "Beast");
    assert_eq!(
        (
            state.objects[&p1_tokens[0]].power,
            state.objects[&p1_tokens[0]].toughness
        ),
        (Some(2), Some(2))
    );
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    let mut expected = vec![p0_tokens[0], p0_tokens[1], p1_tokens[0]];
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        last_created_sorted(state),
        expected,
        "the created referent must be all three copies"
    );
    assert_eq!(
        state.objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
}

/// CR 608.2f + CR 616.1 (B1-NEG): several mandatory replacements with no
/// whole-entry one-shot keep the sequential legs — each seat's immaterial
/// Season/Visitation pair auto-applies to Angels x2. Passes pre- and
/// post-fix; guards the legs path and the compound gate.
#[test]
fn death_by_dragons_two_substitutions_without_entry_copy_stay_sequential() {
    let mut scenario = game();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE_VISITATION);
    scenario.add_enchantment_from_oracle(P1, "Divine Visitation", DIVINE_VISITATION);
    let dragons = scenario
        .add_spell_to_hand_from_oracle(P0, "Death by Dragons", false, DEATH_BY_DRAGONS)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(dragons).target_player(P2).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "both legs must auto-apply without prompting, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "stack must be empty after resolution"
    );
    let state = outcome.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 2, "P0 must create two Angels");
    for id in &p0_tokens {
        assert_angel(state, *id, P0);
    }
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(p1_tokens.len(), 2, "P1 must create two Angels");
    for id in &p1_tokens {
        assert_angel(state, *id, P1);
    }
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_no_dragons(state);
    outcome.assert_zone(&[dragons], Zone::Graveyard);
}
