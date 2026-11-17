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
use engine::game::combat::AttackTarget;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
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
/// replaced, never created. CR 608.2h + CR 614.6: "that many" is the two
/// Soldiers the replaced event would have created, so two Cats are created,
/// untapped like the Soldiers.
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
    let cats: Vec<&GameObject> = p0_tokens(&runner)
        .into_iter()
        .filter(|token| has_subtype(token, "Cat"))
        .collect();
    assert_eq!(cats.len(), 2, "that many: one Cat per replaced Soldier");
    assert!(
        cats.iter().all(|cat| !cat.tapped),
        "the Cats enter untapped"
    );
    assert_eq!(runner.state().objects[&alarm].zone, Zone::Graveyard);
}

/// CR 608.2h: Jinnie's "that many" is bound to the replaced creation, not to
/// the enclosing trigger's event. Primo, the Unbounded (two +1/+1 counters)
/// deals 2 combat damage to P1; its trigger creates one Fractal, which P0's
/// Jinnie replaces with one Cat, and "Put a number of +1/+1 counters on it
/// equal to the damage dealt" still reads the 2 damage.
#[test]
fn jinnie_that_many_does_not_leak_into_primos_damage_dealt() {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let primo = scenario.add_real_card(P0, "Primo, the Unbounded", Zone::Battlefield, db);
    scenario.with_counter(primo, CounterType::Plus1Plus1, 2);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(primo, AttackTarget::Player(P1))])
        .expect("Primo attacks P1");

    let mut reached = false;
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "the prompt is Jinnie's"
                );
                reached = true;
                break;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected window before Jinnie's prompt: {other:?}"),
        }
    }
    assert!(reached, "Primo's Fractal creation reaches Jinnie's prompt");
    assert_eq!(runner.life(P1), 18, "Primo dealt 2 combat damage");

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Jinnie's replacement");
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose the Cat substitute");
    runner.advance_until_stack_empty();

    let tokens = p0_tokens(&runner);
    assert!(
        !tokens.iter().any(|token| has_subtype(token, "Fractal")),
        "the Fractal is replaced"
    );
    let cats: Vec<&GameObject> = tokens
        .into_iter()
        .filter(|token| has_subtype(token, "Cat"))
        .collect();
    let [cat] = cats.as_slice() else {
        panic!("that many: one Cat for the one Fractal, got {cats:?}");
    };
    assert_eq!(
        cat.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "the counters equal the 2 damage dealt"
    );
}

/// Lazotep Plating: "Amass Zombies 1. (Put a +1/+1 counter on an Army you
/// control. It's also a Zombie. If you don't control an Army, create a 0/0
/// black Zombie Army creature token first.)\nYou and permanents you control
/// gain hexproof until end of turn. (You and they can't be the targets of
/// spells or abilities your opponents control.)"
///
/// P0 controls Jinnie and casts Lazotep Plating with no Army: the amass's Army
/// creation pauses on Jinnie's prompt (asked of P0) with the amass and the
/// hexproof instruction still owed.
fn lazotep_at_jinnie_prompt() -> (GameRunner, ObjectId) {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Blue]
            .into_iter()
            .map(|mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
            .collect(),
    );
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let plating = scenario.add_real_card(P0, "Lazotep Plating", Zone::Hand, db);
    let mut runner = scenario.build();
    cast_to_jinnie_prompt(&mut runner, jinnie, plating);
    let WaitingFor::ReplacementChoice { player, .. } = &runner.state().waiting_for else {
        unreachable!("cast_to_jinnie_prompt stopped on Jinnie's prompt");
    };
    assert_eq!(*player, P0, "CR 616.1: the creating player chooses");
    (runner, plating)
}

/// CR 117.2e: no player has priority while a spell resolves, so the answer to
/// its last choice finishes it: the stack and every parked resolution frame
/// are empty and a player has priority.
fn assert_settled_by_the_answer(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        state.stack.is_empty(),
        "the spell finished resolving inside the answer, stack {:?}",
        state.stack
    );
    assert!(
        state.resolution_stack.is_empty(),
        "no resolution work is left parked after the answer: {:?}",
        state.resolution_stack
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a player has priority after the answer, got {:?}",
        state.waiting_for
    );
}

/// R22 — the Lazotep Plating rulings: "You amass 1 and grant hexproof all
/// while Lazotep Plating is resolving. Nothing can happen between the two" and
/// "If you create a Zombie Army token when Lazotep Plating instructs you to
/// amass 1, that token will gain hexproof until end of turn." Accepting Jinnie
/// replaces the Army with a Cat (CR 614.6), so no Army exists to receive the
/// counter (CR 701.47a). The amass's remaining work and the hexproof
/// instruction both finish inside the `ChooseBranch` answer, and the Cat gains
/// hexproof (CR 611.2c).
#[test]
fn accepting_jinnie_on_an_amass_army_finishes_the_spell_in_the_answer() {
    let (mut runner, plating) = lazotep_at_jinnie_prompt();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Jinnie's replacement");
    let WaitingFor::ChooseOneOfBranch { player, .. } = &runner.state().waiting_for else {
        panic!(
            "accepting asks which substitute, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "P0 chooses the substitute");
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose the Cat substitute");
    assert_settled_by_the_answer(&runner);

    assert_eq!(
        runner.state().objects[&plating].zone,
        Zone::Graveyard,
        "Lazotep Plating finished resolving"
    );
    let tokens = p0_tokens(&runner);
    let cats: Vec<&&GameObject> = tokens
        .iter()
        .filter(|token| has_subtype(token, "Cat"))
        .collect();
    let [cat] = cats.as_slice() else {
        panic!("reach: Jinnie replaced the Army with one Cat, got {tokens:?}");
    };
    assert!(
        cat.has_keyword(&Keyword::Hexproof),
        "CR 611.2c: the Cat is a permanent P0 controls when the hexproof resolves"
    );
    assert!(
        !tokens.iter().any(|token| has_subtype(token, "Army")),
        "CR 614.6: the replaced Army is never created"
    );
    assert_eq!(
        cat.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "CR 701.47a: with no Army to choose, nothing receives the counter"
    );
}

/// R22 decline twin — CR 701.47a: declining Jinnie creates the 0/0 Zombie
/// Army, which receives the +1/+1 counter and, per the Lazotep Plating
/// ruling, gains hexproof; the spell finishes inside the decline.
#[test]
fn declining_jinnie_on_an_amass_army_finishes_the_spell_in_the_answer() {
    let (mut runner, plating) = lazotep_at_jinnie_prompt();
    let decline = answer_index(&runner, "Decline");
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("decline Jinnie's replacement");
    assert_settled_by_the_answer(&runner);

    assert_eq!(runner.state().objects[&plating].zone, Zone::Graveyard);
    let tokens = p0_tokens(&runner);
    let [army] = tokens.as_slice() else {
        panic!("exactly the Zombie Army, got {tokens:?}");
    };
    assert!(has_subtype(army, "Army") && has_subtype(army, "Zombie"));
    assert_eq!(
        army.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "CR 701.47a: the Army receives the +1/+1 counter"
    );
    assert!(
        army.has_keyword(&Keyword::Hexproof),
        "the amassed Army gains hexproof"
    );
}
