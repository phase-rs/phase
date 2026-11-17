//! Death by Dragons: "Each player other than target player creates a 5/5 red
//! Dragon creature token with flying." The exclusion anchor declares a
//! cast-time player target (CR 115.1a), so the spell fizzles when its only
//! target is illegal (CR 608.2b, ruling R1); otherwise every non-target player
//! creates exactly one Dragon and the targeted player creates none (ruling R2).
//!
//! Runtime rows drive the real cast pipeline with verbatim Oracle text.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
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

/// Assert the live prompt is a CR 616.1 ordering choice and answer it so the
/// candidate sourced from `first_source` applies first.
fn answer_ordering_putting_first(runner: &mut GameRunner, first_source: ObjectId) {
    let WaitingFor::ReplacementChoice {
        candidates, kind, ..
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

/// CR 608.2f + CR 614.5 + CR 616.1 (F2.1/F2.1b): Doubling Season plus Mystic
/// Reflection on the compound event — the ordering choice applies per batch
/// (CR 616.1f repeat), so P0 gets two Bears and P1 gets one, in either order.
/// Pre-fix the legs run instead and P1 keeps a Dragon.
fn season_reflection_into_bears(season_first: bool) {
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
    runner.cast(mystic).target_object(bears).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
    cast_permanent(&mut runner, season, "Doubling Season");

    let _ = runner.cast(dragons).target_player(P2).resolve();
    // Exactly one ordering choice either way (compound post-fix, P0's leg
    // pre-fix); the assertions below discriminate the two paths.
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                let first = if season_first {
                    season
                } else {
                    // The floating one-shot lives under the ObjectId(0) sentinel.
                    ObjectId(0)
                };
                answer_ordering_putting_first(&mut runner, first);
            }
            other => panic!("unexpected prompt while ordering: {other:?}"),
        }
    }
    assert_quiet_completion(&runner, "the token clause");

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
    let mut expected = vec![p0_tokens[0], p0_tokens[1], p1_tokens[0]];
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        last_created_sorted(state),
        expected,
        "the created referent must be all three Bears"
    );
    assert_eq!(
        state.objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
}

#[test]
fn death_by_dragons_season_and_reflection_apply_per_batch_season_first() {
    season_reflection_into_bears(true);
}

#[test]
fn death_by_dragons_season_and_reflection_apply_per_batch_reflection_first() {
    season_reflection_into_bears(false);
}

/// CR 608.2f + CR 614.5 (F2.2): any optional candidate keeps the legacy
/// sequential legs — Reflection is consumed on the first seat only, so P1
/// keeps a Dragon. Passes pre- and post-fix; guards the legs path.
#[test]
fn death_by_dragons_optional_replacement_keeps_sequential_legs() {
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
    runner.cast(mystic).target_object(bears).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );

    let _ = runner.cast(dragons).target_player(P2).resolve();
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { kind, .. } => match kind {
                ReplacementChoiceKind::Order => {
                    answer_ordering_putting_first(&mut runner, ObjectId(0));
                }
                _ => {
                    // Decline Jinnie Fay: index 0 accepts, index 1 declines.
                    runner
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .expect("decline the optional replacement");
                }
            },
            other => panic!("unexpected prompt on the legs path: {other:?}"),
        }
    }
    assert_quiet_completion(&runner, "the token clause");

    let state = runner.state();
    let p0_tokens = controlled_tokens(state, P0);
    assert_eq!(p0_tokens.len(), 1, "P0 must create one token");
    assert_bears(state, p0_tokens[0], P0);
    let p1_tokens = controlled_tokens(state, P1);
    assert_eq!(
        p1_tokens.len(),
        1,
        "P1 must create one token on the second leg"
    );
    assert_dragon(state, p1_tokens[0], P1);
    assert!(
        controlled_tokens(state, P2).is_empty(),
        "the targeted player creates no tokens"
    );
    assert_eq!(
        state.pending_damage_replacements.len(),
        1,
        "only Mystic Reflection's shield should be floating"
    );
    assert!(
        state.pending_damage_replacements[0].is_consumed,
        "Reflection must be consumed on the first seat only"
    );
    assert_eq!(
        state.objects[&dragons].zone,
        Zone::Graveyard,
        "Death by Dragons must reach the graveyard once the clause completes"
    );
}

/// CR 608.2f + CR 614.5 + CR 616.1 (DUAL): Season plus Reflection into
/// Automaton — one ordering choice, then three per-copy type choices (two for
/// P0's doubled batch, one for P1's), each answered by its own controller.
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
            WaitingFor::ReplacementChoice { .. } => {
                answer_ordering_putting_first(&mut runner, season);
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
