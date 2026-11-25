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
//!
//! The entry-state rows prove an accepted substitute keeps the creating
//! instruction's tapped, attacking and riders (Hero of Bladehold, Kari Zev);
//! the drain rows prove the accept leaves an enclosing continuation's drain in
//! place (Kalitas, Hullbreacher).

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, PendingTokenSubstitute, ReplacementChoiceKind, TokenSubstituteChoice,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::FrameKind;
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

// ---------------------------------------------------------------------------
// Entry state: a substitute keeps the creating instruction's tapped, attacking
// and riders (CR 614.6 + the Jinnie Fay ruling).
// ---------------------------------------------------------------------------

/// Hero of Bladehold + Jinnie Fay on P0's battlefield, ready to attack.
fn hero_jinnie_board() -> (GameRunner, ObjectId, ObjectId) {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hero = scenario.add_real_card(P0, "Hero of Bladehold", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    (scenario.build(), hero, jinnie)
}

/// Declare `hero` attacking P1 and drive to Jinnie's prompt on its trigger.
fn attack_to_jinnie_prompt(runner: &mut GameRunner, hero: ObjectId, jinnie: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hero, AttackTarget::Player(P1))])
        .expect("declare Hero attacking P1");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "the prompt is Jinnie's replacement"
                );
                return;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order Hero's triggers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the trigger");
            }
            other => panic!("unexpected window before Jinnie's prompt: {other:?}"),
        }
    }
    panic!("Hero's trigger never reached Jinnie's replacement prompt");
}

/// Every listed token is tapped and attacking.
fn assert_tapped_attacking(runner: &GameRunner, ids: &[ObjectId]) {
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat is still active");
    for id in ids {
        let token = &runner.state().objects[id];
        assert!(token.tapped, "{id:?} enters tapped");
        assert!(
            combat.attackers.iter().any(|a| a.object_id == *id),
            "{id:?} enters attacking"
        );
    }
}

/// CR 614.6 + the Jinnie Fay ruling ("Anything else specified in the effect
/// creating the tokens ... still applies"): the substitute keeps the creating
/// instruction's tapped and attacking.
#[test]
fn jinnie_substitute_keeps_tapped_and_attacking() {
    let (mut runner, hero, jinnie) = hero_jinnie_board();
    attack_to_jinnie_prompt(&mut runner, hero, jinnie);

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
    assert_settled_by_the_answer(&runner);

    let tokens = p0_tokens(&runner);
    assert!(
        !tokens.iter().any(|token| has_subtype(token, "Soldier")),
        "the original Soldiers are replaced, never created"
    );
    let cats: Vec<ObjectId> = tokens
        .iter()
        .filter(|token| has_subtype(token, "Cat"))
        .map(|token| token.id)
        .collect();
    assert_eq!(cats.len(), 2, "that many: one Cat per replaced Soldier");
    assert_tapped_attacking(&runner, &cats);
}

/// Decline control for the row above: two tapped attacking Soldiers.
#[test]
fn jinnie_declined_keeps_tapped_and_attacking_soldiers() {
    let (mut runner, hero, jinnie) = hero_jinnie_board();
    attack_to_jinnie_prompt(&mut runner, hero, jinnie);

    let decline = answer_index(&runner, "Decline");
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("decline Jinnie's replacement");
    assert_settled_by_the_answer(&runner);

    let soldiers: Vec<ObjectId> = p0_tokens(&runner)
        .iter()
        .filter(|token| has_subtype(token, "Soldier"))
        .map(|token| token.id)
        .collect();
    assert_eq!(soldiers.len(), 2, "declining keeps the two Soldiers");
    assert_tapped_attacking(&runner, &soldiers);
}

/// CR 614.6 + the Jinnie Fay ruling (extended to every substitute): Moonlit
/// Meditation's copies keep the creating instruction's tapped and attacking.
#[test]
fn moonlit_substitute_keeps_tapped_and_attacking() {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hero = scenario.add_real_card(P0, "Hero of Bladehold", Zone::Battlefield, db);
    let moonlit = scenario.add_real_card(P0, "Moonlit Meditation", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.attach_as_bestowed_aura(moonlit, hero);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hero, AttackTarget::Player(P1))])
        .expect("declare Hero attacking P1");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates.iter().any(|c| c.source_id == moonlit),
                    "reach guard: Moonlit's prompt is raised"
                );
                break;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order Hero's triggers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the trigger");
            }
            other => panic!("unexpected window before Moonlit's prompt: {other:?}"),
        }
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Moonlit's replacement");
    // No further prompt: the choice-free substitute rewrote the event at once.
    assert_settled_by_the_answer(&runner);

    let heroes: Vec<ObjectId> = p0_tokens(&runner)
        .iter()
        .filter(|token| token.name == "Hero of Bladehold")
        .map(|token| token.id)
        .collect();
    assert_eq!(
        heroes.len(),
        2,
        "that many: one Hero copy per replaced Soldier"
    );
    assert_tapped_attacking(&runner, &heroes);
}

/// CR 614.6 + CR 603.7a + the Jinnie Fay ruling's own rider example ("Exile
/// that token at end of combat"): the substitute keeps tapped and attacking,
/// and the creating instruction's delayed exile still names it.
#[test]
fn jinnie_substitute_for_ragavan_is_exiled_at_end_of_combat() {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kari = scenario.add_real_card(P0, "Kari Zev, Skyship Raider", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kari, AttackTarget::Player(P1))])
        .expect("declare Kari Zev attacking P1");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "reach guard: Kari Zev's trigger raises Jinnie's prompt"
                );
                break;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the trigger");
            }
            other => panic!("unexpected window before Jinnie's prompt: {other:?}"),
        }
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Jinnie's replacement");
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose the Cat substitute");

    let cats: Vec<ObjectId> = p0_tokens(&runner)
        .iter()
        .filter(|token| has_subtype(token, "Cat"))
        .map(|token| token.id)
        .collect();
    assert_eq!(cats.len(), 1, "one Cat for the one Ragavan");
    assert_tapped_attacking(&runner, &cats);
    assert!(
        !p0_tokens(&runner)
            .iter()
            .any(|token| has_subtype(token, "Monkey")),
        "no Ragavan is created"
    );

    // Cross end of combat: the instruction's delayed exile names the Cat.
    // CR 111.7: an exiled token ceases to exist, so it leaves `objects`
    // entirely — absence plus the ZoneChanged-to-Exile event is the exile.
    let cat = cats[0];
    let mut saw_exile = false;
    for _ in 0..60 {
        let gone = runner
            .state()
            .objects
            .get(&cat)
            .is_none_or(|object| object.zone != Zone::Battlefield);
        if gone {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward end of combat");
                saw_exile = saw_exile
                    || result.events.iter().any(
                        |event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if *object_id == cat),
                    );
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            other => panic!("unexpected window before end of combat: {other:?}"),
        }
    }
    assert!(
        saw_exile,
        "the Cat leaves with a ZoneChanged to Exile at end of combat"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&cat)
            .is_none_or(|object| object.zone == Zone::Exile),
        "the substitute is exiled at end of combat"
    );
}

/// Decline control for the row above: Ragavan enters tapped and attacking and
/// is exiled at end of combat.
#[test]
fn ragavan_declined_keeps_tapped_attacking_and_exile() {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kari = scenario.add_real_card(P0, "Kari Zev, Skyship Raider", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kari, AttackTarget::Player(P1))])
        .expect("declare Kari Zev attacking P1");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "reach guard: Kari Zev's trigger raises Jinnie's prompt"
                );
                break;
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the trigger");
            }
            other => panic!("unexpected window before Jinnie's prompt: {other:?}"),
        }
    }
    let decline = answer_index(&runner, "Decline");
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("decline Jinnie's replacement");

    let ragavans: Vec<ObjectId> = p0_tokens(&runner)
        .iter()
        .filter(|token| has_subtype(token, "Monkey"))
        .map(|token| token.id)
        .collect();
    assert_eq!(ragavans.len(), 1, "declining keeps the one Ragavan");
    assert_tapped_attacking(&runner, &ragavans);

    let ragavan = ragavans[0];
    let mut saw_exile = false;
    for _ in 0..60 {
        let gone = runner
            .state()
            .objects
            .get(&ragavan)
            .is_none_or(|object| object.zone != Zone::Battlefield);
        if gone {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward end of combat");
                saw_exile = saw_exile
                    || result.events.iter().any(
                        |event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if *object_id == ragavan),
                    );
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            other => panic!("unexpected window before end of combat: {other:?}"),
        }
    }
    assert!(
        saw_exile,
        "Ragavan leaves with a ZoneChanged to Exile at end of combat"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&ragavan)
            .is_none_or(|object| object.zone == Zone::Exile),
        "Ragavan is exiled at end of combat"
    );
}

// ---------------------------------------------------------------------------
// Drains: the substitute accept leaves an enclosing continuation's drain in
// place (CR 614.6 + CR 616.1g).
// ---------------------------------------------------------------------------

/// P0 Kalitas + Jinnie, P1 `bears` Grizzly Bears, Pyroclasm in P0's hand with
/// {1}{R} to cast it.
fn kalitas_jinnie_board(bears: usize) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>, ObjectId) {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [ManaType::Red, ManaType::Colorless]
            .into_iter()
            .map(|mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
            .collect(),
    );
    let kalitas = scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let mut bear_ids = Vec::new();
    for _ in 0..bears {
        bear_ids.push(scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db));
    }
    let pyroclasm = scenario.add_real_card(P0, "Pyroclasm", Zone::Hand, db);
    (scenario.build(), kalitas, jinnie, bear_ids, pyroclasm)
}

/// Cast `spell` and drive every Jinnie prompt it raises: accept each one and
/// take the Cat branch. Returns the number of branch prompts answered. Every
/// prompt must be P0's OptionalBranch naming Jinnie, and Kalitas's
/// `ReplacementApplied` must already be visible each time one is answered.
fn accept_every_jinnie_prompt(
    runner: &mut GameRunner,
    jinnie: ObjectId,
    kalitas: ObjectId,
    spell: ObjectId,
) -> usize {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the spell");
    let mut seen: Vec<GameEvent> = Vec::new();
    let mut branch_prompts = 0;
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice {
                player,
                candidates,
                kind,
                ..
            } => {
                assert_eq!(*player, P0, "P0 decides Jinnie's replacement");
                assert_eq!(
                    *kind,
                    ReplacementChoiceKind::OptionalBranch,
                    "each prompt is Jinnie's accept/decline"
                );
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "each prompt names Jinnie's replacement: {candidates:?}"
                );
                assert!(
                    seen.iter().any(
                        |event| matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == kalitas)
                    ),
                    "Kalitas's replacement applied before this Jinnie prompt"
                );
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept Jinnie's replacement");
                seen.extend(result.events);
            }
            WaitingFor::ChooseOneOfBranch { player, .. } => {
                assert_eq!(*player, P0, "P0 chooses the substitute");
                assert!(
                    runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .is_some_and(|pending| matches!(
                            pending.token_substitute,
                            Some(PendingTokenSubstitute {
                                awaits: TokenSubstituteChoice::Branch,
                                ..
                            })
                        )),
                    "the accept parks the branch substitute"
                );
                branch_prompts += 1;
                let result = runner
                    .act(GameAction::ChooseBranch { index: 0 })
                    .expect("choose the Cat substitute");
                seen.extend(result.events);
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() {
                    break;
                }
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                seen.extend(result.events);
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    branch_prompts
}

/// No `PostReplacement` frame lingers on the resolution stack.
fn assert_no_post_replacement_frame(runner: &GameRunner) {
    assert!(
        !runner
            .state()
            .resolution_stack
            .iter()
            .any(|frame| frame.kind() == FrameKind::PostReplacement),
        "no PostReplacement frame lingers, frames {:?}",
        runner
            .state()
            .resolution_stack
            .iter()
            .map(|frame| frame.kind())
            .collect::<Vec<_>>()
    );
}

/// CR 614.6 + CR 616.1g + CR 704.3 + CR 704.5g: accepting Jinnie inside
/// Kalitas's continuations keeps both exiles — the accept leaves the enclosing
/// drain in place, and the second death is re-derived by the SBA fixpoint
/// after the first answer.
#[test]
fn jinnie_accepted_inside_kalitas_continuations_keeps_both_exiles() {
    let (mut runner, kalitas, jinnie, bears, pyroclasm) = kalitas_jinnie_board(2);
    assert_eq!(bears.len(), 2);

    let branch_prompts = accept_every_jinnie_prompt(&mut runner, jinnie, kalitas, pyroclasm);
    assert_eq!(
        branch_prompts, 2,
        "exactly two branch prompts: one per replaced death"
    );
    assert_settled_by_the_answer(&runner);
    assert_no_post_replacement_frame(&runner);

    for bear in &bears {
        assert_eq!(
            runner.state().objects[bear].zone,
            Zone::Exile,
            "Kalitas exiles each Bear it replaces"
        );
    }
    let tokens = p0_tokens(&runner);
    assert!(
        !tokens.iter().any(|token| has_subtype(token, "Zombie")),
        "no Zombie survives: each is replaced by a Cat"
    );
    let cats: Vec<ObjectId> = tokens
        .iter()
        .filter(|token| has_subtype(token, "Cat"))
        .map(|token| token.id)
        .collect();
    assert_eq!(cats.len(), 2, "P0's tokens are exactly two Cats");
    assert_eq!(
        runner.state().objects[&kalitas].zone,
        Zone::Battlefield,
        "Kalitas survives its own Pyroclasm"
    );
    assert_eq!(
        runner.state().objects[&jinnie].zone,
        Zone::Battlefield,
        "Jinnie survives the Pyroclasm"
    );
    assert_eq!(
        runner.state().objects[&pyroclasm].zone,
        Zone::Graveyard,
        "Pyroclasm finished resolving"
    );
}

/// One-creature sibling of the row above.
#[test]
fn jinnie_accepted_inside_a_kalitas_continuation_keeps_the_exile() {
    let (mut runner, kalitas, jinnie, bears, pyroclasm) = kalitas_jinnie_board(1);
    assert_eq!(bears.len(), 1);

    let branch_prompts = accept_every_jinnie_prompt(&mut runner, jinnie, kalitas, pyroclasm);
    assert_eq!(branch_prompts, 1, "one branch prompt for the one death");
    assert_settled_by_the_answer(&runner);
    assert_no_post_replacement_frame(&runner);

    assert_eq!(
        runner.state().objects[&bears[0]].zone,
        Zone::Exile,
        "Kalitas exiles the Bear it replaces"
    );
    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 1, "P0's token is exactly one Cat");
    assert!(has_subtype(tokens[0], "Cat"));
    assert_eq!(
        runner.state().objects[&pyroclasm].zone,
        Zone::Graveyard,
        "Pyroclasm finished resolving"
    );
}

/// Decline control (attribution) for the rows above: declining Jinnie keeps
/// both Kalitas Zombies. If this row is red, the board is wrong, not the
/// accept.
#[test]
fn jinnie_declined_inside_kalitas_continuations_keeps_both_zombies() {
    let (mut runner, kalitas, jinnie, bears, pyroclasm) = kalitas_jinnie_board(2);
    assert_eq!(bears.len(), 2);
    let _ = kalitas;

    let card_id = runner.state().objects[&pyroclasm].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: pyroclasm,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Pyroclasm");
    let mut declines = 0;
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "each prompt names Jinnie's replacement: {candidates:?}"
                );
                let decline = answer_index(&runner, "Decline");
                runner
                    .act(GameAction::ChooseReplacement { index: decline })
                    .expect("decline Jinnie's replacement");
                declines += 1;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    assert_eq!(declines, 2, "both Zombies are offered to Jinnie");
    assert_settled_by_the_answer(&runner);

    for bear in &bears {
        assert_eq!(
            runner.state().objects[bear].zone,
            Zone::Exile,
            "Kalitas exiles each Bear"
        );
    }
    let tokens = p0_tokens(&runner);
    assert_eq!(tokens.len(), 2, "both Zombies are created");
    assert!(
        tokens.iter().all(|token| has_subtype(token, "Zombie")
            && token.power == Some(2)
            && token.toughness == Some(2)),
        "both are Kalitas's 2/2 Zombies"
    );
}

fn hand_len(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn library_len(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .len()
}

/// CR 121.6 + CR 614.11 + CR 614.6: accepting Jinnie on Hullbreacher's
/// Treasures keeps the draw instruction — the accept leaves the draw
/// continuation in place, so the second replaced draw still prompts.
#[test]
fn jinnie_accepted_on_hullbreachers_treasures_keeps_the_draw_instruction() {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect(),
    );
    let hullbreacher = scenario.add_real_card(P0, "Hullbreacher", Zone::Battlefield, db);
    let jinnie = scenario.add_real_card(P0, JINNIE, Zone::Battlefield, db);
    let inspiration = scenario.add_real_card(P0, "Inspiration", Zone::Hand, db);
    let mut runner = scenario.build();
    let _ = hullbreacher;

    let p1_hand = hand_len(runner.state(), P1);
    let p1_library = library_len(runner.state(), P1);

    let _ = runner.cast(inspiration).target_player(P1).resolve();
    let mut branch_prompts = 0;
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice {
                player,
                candidates,
                kind,
                ..
            } => {
                assert_eq!(*player, P0, "P0 decides Jinnie's replacement");
                assert_eq!(
                    *kind,
                    ReplacementChoiceKind::OptionalBranch,
                    "each prompt is Jinnie's accept/decline"
                );
                assert!(
                    candidates.iter().any(|c| c.source_id == jinnie),
                    "each prompt names Jinnie's replacement: {candidates:?}"
                );
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept Jinnie's replacement");
            }
            WaitingFor::ChooseOneOfBranch { player, .. } => {
                assert_eq!(*player, P0, "P0 chooses the substitute");
                branch_prompts += 1;
                runner
                    .act(GameAction::ChooseBranch { index: 0 })
                    .expect("choose the Cat substitute");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    assert_eq!(
        branch_prompts, 2,
        "reach guard: one Jinnie prompt per replaced draw"
    );
    assert_settled_by_the_answer(&runner);
    assert_no_post_replacement_frame(&runner);

    assert_eq!(
        hand_len(runner.state(), P1),
        p1_hand,
        "P1's hand is unchanged: both draws were replaced"
    );
    assert_eq!(
        library_len(runner.state(), P1),
        p1_library,
        "P1's library is unchanged: both draws were replaced"
    );
    let tokens = p0_tokens(&runner);
    assert!(
        !tokens.iter().any(|token| token.name == "Treasure"),
        "no Treasure survives: each is replaced by a Cat"
    );
    let cats: Vec<ObjectId> = tokens
        .iter()
        .filter(|token| has_subtype(token, "Cat"))
        .map(|token| token.id)
        .collect();
    assert_eq!(cats.len(), 2, "P0's tokens are exactly two Cats");
    assert!(
        runner.state().active_draw_sequence().is_none(),
        "the draw instruction left no draw sequence behind"
    );
    assert_eq!(
        runner.state().objects[&inspiration].zone,
        Zone::Graveyard,
        "Inspiration finished resolving"
    );
}
