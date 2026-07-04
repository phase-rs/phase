//! Issue #4379: canceling during convoke payment for an X spell must untap
//! creatures tapped toward convoke (Chord of Calling, X=3).

use engine::game::scenario::GameScenario;
use engine::game::EngineError;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const CHORD_ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each \
creature you tap while casting this spell pays for {1} or one mana of that creature's \
color.)\nSearch your library for a creature card with mana value X or less, put it \
onto the battlefield, then shuffle.";

fn chord_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::X,
            ManaCostShard::Green,
            ManaCostShard::Green,
            ManaCostShard::Green,
        ],
        generic: 0,
    }
}

#[test]
fn chord_x3_convoke_tap_then_cancel_untaps_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let convoker = scenario.add_creature(P0, "Convoker", 1, 1).id();
    // Additional convokers so X=3 is affordable via convoke alone (CR 601.2f).
    let extra_convokers: Vec<_> = (0..6)
        .map(|i| scenario.add_creature(P0, &format!("Helper {i}"), 1, 1).id())
        .collect();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let mut runner = scenario.build();

    for &id in std::iter::once(&convoker).chain(extra_convokers.iter()) {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .color
            .push(ManaColor::Green);
    }

    assert!(
        runner.state().objects[&spell_id]
            .keywords
            .contains(&Keyword::Convoke),
        "Chord must carry Convoke"
    );

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell");

    // CR 601.2f: announce X=3.
    match &runner.state().waiting_for {
        WaitingFor::ChooseXValue { .. } => {}
        other => panic!("expected ChooseXValue, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseX { value: 3 })
        .expect("ChooseX");

    match &runner.state().waiting_for {
        WaitingFor::ManaPayment {
            convoke_mode: Some(_),
            ..
        } => {}
        other => panic!("expected ManaPayment with convoke, got {other:?}"),
    }

    runner
        .act(GameAction::TapForConvoke {
            object_id: convoker,
            mana_type: ManaType::Colorless,
        })
        .expect("TapForConvoke");

    assert!(
        runner.state().objects[&convoker].tapped,
        "creature should be tapped after convoke contribution"
    );

    runner.act(GameAction::CancelCast).expect("CancelCast");

    assert!(
        !runner.state().objects[&convoker].tapped,
        "cancel must untap creature tapped for convoke (issue #4379)"
    );
    assert!(
        runner.state().players[0].hand.contains(&spell_id),
        "spell must return to hand after cancel"
    );
    assert!(
        runner.state().players[0].mana_pool.mana.is_empty(),
        "convoke payment markers must be cleared on cancel"
    );
    assert!(
        runner.state().pending_cast.is_none(),
        "pending cast must be cleared"
    );
}

/// CR 601.2h: Pay (`PassPriority`) with insufficient mana must not leave
/// convoke-tapped creatures tapped or drop `pending_cast` without cleanup.
#[test]
fn chord_x3_convoke_tap_then_pass_priority_with_insufficient_mana_untaps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let convoker = scenario.add_creature(P0, "Convoker", 1, 1).id();
    let extra_convokers: Vec<_> = (0..6)
        .map(|i| scenario.add_creature(P0, &format!("Helper {i}"), 1, 1).id())
        .collect();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let mut runner = scenario.build();

    for &id in std::iter::once(&convoker).chain(extra_convokers.iter()) {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .color
            .push(ManaColor::Green);
    }

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell");
    runner
        .act(GameAction::ChooseX { value: 3 })
        .expect("ChooseX");

    runner
        .act(GameAction::TapForConvoke {
            object_id: convoker,
            mana_type: ManaType::Colorless,
        })
        .expect("TapForConvoke");
    assert!(runner.state().objects[&convoker].tapped);

    let pay_err = runner
        .act(GameAction::PassPriority)
        .expect_err("Pay with partial convoke must fail");
    assert!(
        matches!(pay_err, EngineError::ActionNotAllowed(_)),
        "expected ActionNotAllowed, got {pay_err:?}"
    );

    assert!(
        runner.state().objects[&convoker].tapped,
        "failed Pay must leave convoke tap in place (action did not succeed)"
    );
    assert!(
        runner.state().pending_cast.is_some(),
        "failed Pay must restore pending cast so CancelCast remains available"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("CancelCast after failed Pay");

    assert!(
        !runner.state().objects[&convoker].tapped,
        "CancelCast after failed Pay must untap convoke creatures (issue #4379)"
    );
}

/// Reproduces Discord order: Pay first (fails), then convoke tap attempt.
#[test]
fn chord_x3_pass_priority_then_convoke_tap_then_cancel_untaps_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let convoker = scenario.add_creature(P0, "Convoker", 1, 1).id();
    let extra_convokers: Vec<_> = (0..6)
        .map(|i| scenario.add_creature(P0, &format!("Helper {i}"), 1, 1).id())
        .collect();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let mut runner = scenario.build();

    for &id in std::iter::once(&convoker).chain(extra_convokers.iter()) {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .color
            .push(ManaColor::Green);
    }

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell");
    runner
        .act(GameAction::ChooseX { value: 3 })
        .expect("ChooseX");

    // Click Pay before convoke-tapping (insufficient mana — must error or stay in payment).
    let pay_result = runner.act(GameAction::PassPriority);
    if pay_result.is_ok() {
        // If payment somehow succeeded, tap convoke afterward then cancel.
        let _ = runner.act(GameAction::TapForConvoke {
            object_id: convoker,
            mana_type: ManaType::Colorless,
        });
    } else {
        runner
            .act(GameAction::TapForConvoke {
                object_id: convoker,
                mana_type: ManaType::Colorless,
            })
            .expect("TapForConvoke after failed Pay");
    }

    runner.act(GameAction::CancelCast).expect("CancelCast");

    assert!(
        !runner.state().objects[&convoker].tapped,
        "cancel must untap after Pay-then-convoke-tap sequence (issue #4379)"
    );
}
