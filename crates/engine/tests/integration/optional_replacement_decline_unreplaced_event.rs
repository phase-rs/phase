//! A declined optional replacement leaves its event unreplaced.
//!
//! CR 614.1a makes "you may instead" a replacement effect, and CR 614.6 says
//! only a replaced event "never happens". When the player declines the "may",
//! nothing replaces the event, so the original event happens as it would have
//! without the effect. Jinnie Fay, Jetmir's Second is the printed member of this
//! class whose accept-side outcome (the chosen token substitution) the token
//! applier also applied on decline: the original token count was set to zero
//! and no substitute ran, so declining created no tokens at all.
//!
//! These rows drive the real card data through `GameAction`s: cast Raise the
//! Alarm, answer Jinnie's prompt, and read the battlefield. The accept row is
//! the reach guard for the decline row: it proves the same prompt really
//! applies Jinnie's replacement (it asks for a substitute and no Soldier is
//! created), so the decline row cannot pass by Jinnie never applying.

use engine::database::card_db::CardDatabase;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const JINNIE: &str = "Jinnie Fay, Jetmir's Second";

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

/// Jinnie on P0's battlefield, Raise the Alarm in P0's hand and the {1}{W} to
/// cast it.
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let alarm = scenario.add_real_card(P0, "Raise the Alarm", Zone::Hand, db);
    (scenario.build(), jinnie, alarm)
}

/// Cast Raise the Alarm and pass priority until Jinnie's optional replacement
/// asks P0 whether to apply it.
fn cast_to_jinnie_prompt(runner: &mut GameRunner, jinnie: ObjectId, alarm: ObjectId) {
    let card_id = runner.state().objects[&alarm].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: alarm,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Raise the Alarm");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| candidate.source_id == jinnie),
                    "the prompt must be Jinnie's replacement"
                );
                return;
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward resolution");
            }
            other => panic!("unexpected window before Jinnie's prompt: {other:?}"),
        }
    }
    panic!("Raise the Alarm never reached Jinnie's replacement prompt");
}

fn answer_index(runner: &GameRunner, description: &str) -> usize {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("expected Jinnie's replacement prompt");
    };
    candidates
        .iter()
        .position(|candidate| candidate.description == description)
        .unwrap_or_else(|| panic!("no {description:?} answer among {candidates:?}"))
}

fn p0_tokens(runner: &GameRunner) -> Vec<&GameObject> {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.is_token && obj.controller == P0)
        .collect()
}

fn has_subtype(obj: &GameObject, subtype: &str) -> bool {
    obj.card_types.subtypes.iter().any(|s| s == subtype)
}

/// CR 614.6: declining Jinnie's "may instead" replaces nothing — Raise the
/// Alarm's two Soldier tokens are created, and no Cat or Dog.
#[test]
fn declining_jinnie_creates_the_original_tokens() {
    let (mut runner, jinnie, alarm) = setup();
    cast_to_jinnie_prompt(&mut runner, jinnie, alarm);

    let decline = answer_index(&runner, "Decline");
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("decline Jinnie's replacement");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. } | WaitingFor::ChooseOneOfBranch { .. }
        ),
        "a declined replacement asks nothing more, got {:?}",
        runner.state().waiting_for
    );
    let tokens = p0_tokens(&runner);
    assert_eq!(
        tokens.len(),
        2,
        "declining keeps the original event: exactly Raise the Alarm's two tokens"
    );
    assert!(
        tokens.iter().all(|token| has_subtype(token, "Soldier")
            && token.power == Some(1)
            && token.toughness == Some(1)),
        "both are the original 1/1 Soldiers"
    );
    assert!(
        !tokens
            .iter()
            .any(|token| has_subtype(token, "Cat") || has_subtype(token, "Dog")),
        "no substitute token is created on decline"
    );
    assert_eq!(runner.state().objects[&alarm].zone, Zone::Graveyard);
}

/// The accept polarity of the same prompt, as the decline row's reach guard:
/// accepting asks which substitute to create, and the original Soldiers are
/// replaced, never created. The substitutes' "that many" count is resolved by
/// the branch continuation, not by the decline seam these rows pin, so it is not
/// asserted here.
#[test]
fn accepting_jinnie_replaces_the_original_tokens() {
    let (mut runner, jinnie, alarm) = setup();
    cast_to_jinnie_prompt(&mut runner, jinnie, alarm);

    // An optional replacement prompt answers accept at index 0 and decline at 1.
    assert_eq!(answer_index(&runner, "Decline"), 1);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Jinnie's replacement");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "accepting asks which substitute, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose the Cat substitute");
    runner.advance_until_stack_empty();

    assert!(
        !p0_tokens(&runner)
            .iter()
            .any(|token| has_subtype(token, "Soldier")),
        "the original Soldiers are replaced, never created"
    );
    assert_eq!(runner.state().objects[&alarm].zone, Zone::Graveyard);
}
