//! Regression for GitHub issue #3263 — Gitaxian Probe's "look at target player's
//! hand" must be private to the caster (CR 701.20e), not a public reveal.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetRef};
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GITAXIAN_PROBE: &str = "Look at target player's hand.\nDraw a card.";

#[test]
fn gitaxian_probe_self_look_does_not_leak_hand_to_opponent() {
    let def = parse_effect_chain(GITAXIAN_PROBE, AbilityKind::Spell);
    let Effect::RevealHand { reveal, .. } = def.effect.as_ref() else {
        panic!("expected RevealHand head, got {:?}", def.effect);
    };
    assert!(
        !*reveal,
        "Gitaxian Probe look-at-hand must parse as a private look"
    );

    let mut state = GameState::new_two_player(3263);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Gitaxian Probe".to_string(),
        Zone::Stack,
    );
    let secret_card = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Probe Secret".to_string(),
        Zone::Hand,
    );
    create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "Drawn Card".to_string(),
        Zone::Library,
    );

    let ability = build_resolved_from_def_with_targets(
        &def,
        source,
        PlayerId(1),
        vec![TargetRef::Player(PlayerId(1))],
    );
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(
        !state.revealed_cards.contains(&secret_card),
        "self-target look must not publish hand cards via revealed_cards"
    );
    assert_eq!(state.private_look_player, Some(PlayerId(1)));

    let opponent_view = filter_state_for_viewer(&state, PlayerId(0));
    assert_eq!(
        opponent_view.objects[&secret_card].name, "Hidden Card",
        "opponent must not see cards from a self-target Gitaxian Probe look"
    );

    let caster_view = filter_state_for_viewer(&state, PlayerId(1));
    assert_eq!(
        caster_view.objects[&secret_card].name, "Probe Secret",
        "caster still sees their own hand after looking"
    );
    assert_eq!(
        state.players[1].hand.len(),
        2,
        "look + draw should leave the original hand card and add one drawn card"
    );
    assert!(
        state.players[1].hand.contains(&secret_card),
        "looked-at card must remain in hand"
    );
}
