//! Regression for GitHub issue #3263 — Gitaxian Probe's "look at target player's
//! hand" must be private to the caster (CR 701.20e), not a public reveal.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, ShardChoice, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GITAXIAN_PROBE: &str = "Look at target player's hand.\nDraw a card.";

#[test]
fn gitaxian_probe_paid_life_keeps_target_hand_known_after_opponent_land_play() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let probe = scenario
        .add_spell_to_hand_from_oracle(P0, "Gitaxian Probe", true, GITAXIAN_PROBE)
        .id();
    let secret = scenario.add_creature_to_hand(P1, "Probe Secret", 2, 2).id();
    let land = scenario.add_land_to_hand(P1, "Forest").id();
    let mut runner = scenario.build();
    create_object(
        runner.state_mut(),
        CardId(900),
        P0,
        "Probe Draw".to_string(),
        Zone::Library,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&probe)
        .unwrap()
        .mana_cost = ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::PhyrexianBlue],
    };

    let card_id = runner.state().objects[&probe].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: probe,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Gitaxian Probe through cast pipeline");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("submit target player");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PhyrexianPayment { .. }
    ));
    runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife],
        })
        .expect("pay Gitaxian Probe's Phyrexian mana with life");
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Probe normally");
    }
    assert_eq!(
        filter_state_for_viewer(runner.state(), P0).objects[&secret].name,
        "Probe Secret"
    );
    // The opponent's normal land play happens in their next main phase. Configure
    // that legal priority window directly; advancing an entire turn is unrelated
    // to the action-boundary regression under test.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id: runner.state().objects[&land].card_id,
        })
        .expect("opponent normal land play");
    assert_eq!(
        filter_state_for_viewer(runner.state(), P0).objects[&secret].name,
        "Probe Secret"
    );
    assert_eq!(
        filter_state_for_viewer(runner.state(), P1).objects[&secret].name,
        "Probe Secret"
    );
}

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
