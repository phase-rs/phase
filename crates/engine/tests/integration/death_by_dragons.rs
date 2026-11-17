//! Death by Dragons: "Each player other than target player creates a 5/5 red
//! Dragon creature token with flying." The exclusion anchor declares a
//! cast-time player target (CR 115.1a), so the spell fizzles when its only
//! target is illegal (CR 608.2b, ruling R1); otherwise every non-target player
//! creates exactly one Dragon and the targeted player creates none (ruling R2).
//!
//! Runtime rows drive the real cast pipeline with verbatim Oracle text.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
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
    const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.\nForetell {U} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
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
