//! Runtime pipeline regressions for the Onslaught "Words of" cycle.
//!
//! Each card reads "{1}: The next time you would draw a card this turn,
//! [substitute] instead." Activating one resolves into a one-shot, this-turn
//! draw replacement (CR 614.1a + CR 614.6 + CR 514.2). These tests activate the
//! real Oracle text through `GameAction`s, then make the controller draw with a
//! separate "{T}: Draw a card." permanent, and assert the substitute ran in
//! place of the draw.
//!
//! Words of War is the one member whose substitute names a target. CR 115.1c:
//! an activated ability that uses "target" (CR 115.4: "any target") is
//! targeted, and its target is chosen as the ability is activated (CR 602.2b),
//! then carried into the replacement effect it creates.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WORDS_OF_WAR: &str =
    "{1}: The next time you would draw a card this turn, this enchantment deals 2 damage to any target instead.";
const WORDS_OF_WIND: &str =
    "{1}: The next time you would draw a card this turn, each player returns a permanent they control to its owner's hand instead.";
const WORDS_OF_WASTE: &str =
    "{1}: The next time you would draw a card this turn, each opponent discards a card instead.";
const WORDS_OF_WILDING: &str =
    "{1}: The next time you would draw a card this turn, create a 2/2 green Bear creature token instead.";
const WORDS_OF_WORSHIP: &str =
    "{1}: The next time you would draw a card this turn, you gain 5 life instead.";

const DRAW_A_CARD: &str = "{T}: Draw a card.";

fn colorless_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// P0 controls the named Words enchantment and a "{T}: Draw a card." artifact,
/// with `mana` colorless in pool and a known library top. P1 has a padded
/// library so no one decks out.
struct Setup {
    scenario: GameScenario,
    words: ObjectId,
    drawer: ObjectId,
    library_top: ObjectId,
}

fn setup(name: &str, oracle: &str, mana: usize) -> Setup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library A", "P1 Library B"]);
    let library_top = scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario.add_enchantment_from_oracle(P0, name, oracle).id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Test Drawing Artifact", DRAW_A_CARD)
        .id();
    scenario.with_mana_pool(P0, colorless_mana(mana));
    Setup {
        scenario,
        words,
        drawer,
        library_top,
    }
}

fn first_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    activated_ability_definitions(runner.state(), source)
        .into_iter()
        .next()
        .expect("source has an activated ability")
        .0
}

fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.resolve_top();
    }
}

/// Activate the draw artifact and resolve it: P0 would draw one card.
fn draw_one(runner: &mut GameRunner, drawer: ObjectId) {
    let index = first_ability_index(runner, drawer);
    runner.activate(drawer, index).resolve();
    resolve_stack(runner);
}

fn in_hand(runner: &GameRunner, object: ObjectId) -> bool {
    runner.state().objects[&object].zone == Zone::Hand
}

/// CR 115.1c + CR 602.2b + CR 614.6: Words of War's target is chosen as the
/// ability is activated; the next draw is replaced by 2 damage to that
/// creature, and no card is drawn.
#[test]
fn words_of_war_damages_targeted_creature_instead_of_drawing() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).target_object(bear).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);

    assert!(
        !in_hand(&runner, library_top),
        "CR 614.6: the draw is replaced — the library top stays in the library"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "the targeted 2/2 takes 2 damage and dies (CR 704.5g)"
    );
}

/// CR 115.4: "any target" includes players — the chosen player takes 2.
#[test]
fn words_of_war_damages_targeted_player_instead_of_drawing() {
    let Setup {
        scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let mut runner = scenario.build();
    let p1_life = runner.state().players[1].life;

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).target_player(P1).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert_eq!(
        runner.state().players[1].life,
        p1_life - 2,
        "the targeted opponent takes 2 damage instead of the draw"
    );
}

/// CR 115.1c: activating Words of War surfaces a target prompt at activation.
#[test]
fn words_of_war_activation_prompts_for_a_target() {
    let Setup {
        scenario, words, ..
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner
        .act(GameAction::ActivateAbility {
            source_id: words,
            ability_index: index,
        })
        .expect("activation accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "Words of War must ask for its any-target at activation, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 614.6: if the chosen target is gone by the time the draw would happen,
/// the draw is still replaced — the damage instruction simply can't be carried
/// out and is ignored.
#[test]
fn words_of_war_still_replaces_draw_when_target_left() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let p1_life = runner.state().players[1].life;

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).target_object(bear).resolve();
    resolve_stack(&mut runner);

    engine::game::zones::move_to_zone(runner.state_mut(), bear, Zone::Hand, &mut Vec::new());

    draw_one(&mut runner, drawer);

    assert!(
        !in_hand(&runner, library_top),
        "CR 614.6: the draw is still replaced"
    );
    assert_eq!(
        runner.state().players[1].life,
        p1_life,
        "the damage is not redirected anywhere else"
    );
}

/// CR 608.2b: if Words of War's only target is illegal as the ability tries to
/// resolve, the ability doesn't resolve — no shield is installed and the next
/// draw is a normal draw.
#[test]
fn words_of_war_fizzles_when_target_illegal_on_resolution() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // Drive the activation by hand so the ability stays on the stack.
    let index = first_ability_index(&runner, words);
    runner
        .act(GameAction::ActivateAbility {
            source_id: words,
            ability_index: index,
        })
        .expect("activation accepted");
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating: {other:?}"),
        };
        runner.act(action).expect("activation step accepted");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Words of War's ability is on the stack"
    );
    engine::game::zones::move_to_zone(runner.state_mut(), bear, Zone::Hand, &mut Vec::new());
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);

    assert!(
        in_hand(&runner, library_top),
        "no shield was installed, so the draw happens normally"
    );
}

/// CR 113.7a: the shield exists independently of Words of War once the ability
/// resolved — the damage is still dealt if the enchantment has left the
/// battlefield before the draw.
#[test]
fn words_of_war_damages_after_source_left() {
    let Setup {
        scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let mut runner = scenario.build();
    let p1_life = runner.state().players[1].life;

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).target_player(P1).resolve();
    resolve_stack(&mut runner);
    engine::game::zones::move_to_zone(runner.state_mut(), words, Zone::Graveyard, &mut Vec::new());

    draw_one(&mut runner, drawer);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert_eq!(runner.state().players[1].life, p1_life - 2);
}

/// CR 614.6: the damage shield is one-shot — the second draw is normal.
#[test]
fn words_of_war_second_draw_is_normal() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let second = scenario.add_card_to_library_top(P0, "P0 Second");
    let second_drawer = scenario
        .add_artifact_from_oracle(P0, "Second Drawing Artifact", DRAW_A_CARD)
        .id();
    let mut runner = scenario.build();
    let p1_life = runner.state().players[1].life;

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).target_player(P1).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);
    draw_one(&mut runner, second_drawer);

    assert_eq!(runner.state().players[1].life, p1_life - 2);
    assert!(
        in_hand(&runner, second),
        "the first draw was replaced, so the second draw takes the top card"
    );
    assert!(!in_hand(&runner, library_top));
}

/// Words of Wind: each player returns a permanent they control to its owner's
/// hand instead of the draw.
#[test]
fn words_of_wind_each_player_bounces_a_permanent_instead_of_drawing() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of Wind", WORDS_OF_WIND, 1);
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);
    answer_choices_with_first_legal(&mut runner);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert_eq!(
        runner.state().objects[&p1_bear].zone,
        Zone::Hand,
        "P1's only permanent is returned to its owner's hand"
    );
    let p0_bounced = runner.state().players[0]
        .hand
        .iter()
        .filter(|id| **id == words || **id == drawer)
        .count();
    assert_eq!(
        p0_bounced, 1,
        "P0 returns exactly one permanent it controls"
    );
}

/// Words of Waste: each opponent discards a card instead of the draw.
#[test]
fn words_of_waste_each_opponent_discards_instead_of_drawing() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of Waste", WORDS_OF_WASTE, 1);
    let p1_card = scenario.add_creature_to_hand(P1, "P1 Hand Card", 1, 1).id();
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);
    answer_choices_with_first_legal(&mut runner);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert_eq!(
        runner.state().objects[&p1_card].zone,
        Zone::Graveyard,
        "the opponent discards their only card"
    );
}

/// Words of Wilding: a 2/2 green Bear token is created instead of the draw.
#[test]
fn words_of_wilding_creates_bear_instead_of_drawing() {
    let Setup {
        scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of Wilding", WORDS_OF_WILDING, 1);
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    let bears = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &runner.state().objects[*id];
            obj.controller == P0
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && obj.card_types.subtypes.iter().any(|s| s == "Bear")
        })
        .count();
    assert_eq!(bears, 1, "one Bear token is created instead of the draw");
}

/// Words of Worship: gain 5 life instead of the draw.
#[test]
fn words_of_worship_gains_life_instead_of_drawing() {
    let Setup {
        scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of Worship", WORDS_OF_WORSHIP, 1);
    let mut runner = scenario.build();
    let life = runner.state().players[0].life;

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);

    draw_one(&mut runner, drawer);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert_eq!(runner.state().players[0].life, life + 5);
}

/// Ruling (2004-10-04): "If multiple Words have been used prior to drawing a
/// card, then you can choose which one to apply (and use up) each time you draw
/// a card." CR 616.1: the affected player chooses among the applicable
/// replacement effects; each is one-shot (CR 614.6), so the second draw uses the
/// other.
#[test]
fn multiple_words_let_the_player_choose_which_applies_per_draw() {
    let Setup {
        mut scenario,
        words: worship,
        drawer,
        library_top,
    } = setup("Words of Worship", WORDS_OF_WORSHIP, 2);
    let wilding = scenario
        .add_enchantment_from_oracle(P0, "Words of Wilding", WORDS_OF_WILDING)
        .id();
    let second_drawer = scenario
        .add_artifact_from_oracle(P0, "Second Drawing Artifact", DRAW_A_CARD)
        .id();
    let mut runner = scenario.build();
    let life = runner.state().players[0].life;
    let bear_count = |runner: &GameRunner| {
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                runner.state().objects[*id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s == "Bear")
            })
            .count()
    };

    for words in [worship, wilding] {
        let index = first_ability_index(&runner, words);
        runner.activate(words, index).resolve();
        resolve_stack(&mut runner);
    }

    let applied = |runner: &GameRunner| {
        usize::from(runner.state().players[0].life == life + 5) + bear_count(runner)
    };

    // First draw: both shields apply, so P0 is asked which one to use.
    let index = first_ability_index(&runner, drawer);
    runner.activate(drawer, index).resolve();
    resolve_stack(&mut runner);
    let WaitingFor::ReplacementChoice {
        candidate_count, ..
    } = runner.state().waiting_for
    else {
        panic!(
            "two applicable Words must prompt a CR 616.1 choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(candidate_count, 2, "both Words shields apply to the draw");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement choice accepted");
    resolve_stack(&mut runner);
    assert_eq!(applied(&runner), 1, "exactly one Words is used up");

    // Second draw: only the other shield remains, and it applies.
    let index = first_ability_index(&runner, second_drawer);
    runner.activate(second_drawer, index).resolve();
    resolve_stack(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "one remaining shield needs no choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        applied(&runner),
        2,
        "the other Words is used on the next draw"
    );
    assert!(
        !in_hand(&runner, library_top),
        "both draws were replaced, one by each Words"
    );
}

/// Answer any resolution-time card/permanent choices with the first legal
/// option until priority returns. Used for the "each player returns a
/// permanent" / "each opponent discards" substitutes, whose picks are made by
/// the affected players while the replacement applies (CR 115.10 — not
/// targets).
fn answer_choices_with_first_legal(runner: &mut GameRunner) {
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::DiscardChoice { cards, count, .. } => GameAction::SelectCards {
                cards: cards.iter().copied().take(*count).collect(),
            },
            WaitingFor::EffectZoneChoice { cards, count, .. } => GameAction::SelectCards {
                cards: cards.iter().copied().take(*count).collect(),
            },
            other => panic!("unexpected prompt while applying the substitute: {other:?}"),
        };
        runner.act(action).expect("choice accepted");
        resolve_stack(runner);
    }
}
