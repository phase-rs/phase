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
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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
const CURFEW: &str = "Each player returns a creature they control to its owner's hand.";
const TRADEWIND_RIDER_AVATAR: &str =
    "{3}: Each player returns a permanent they control to its owner's hand unless they pay 2 life.";

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
            payment_mode: CastPaymentMode::Auto,
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
    assert_eq!(runner.state().objects[&bear].damage_marked, 0);
}

/// CR 400.7 + CR 614.6: the stored substitute keeps the announced target's
/// identity; blinking it after shield installation does not make its new
/// incarnation a damage recipient, while the next draw is still replaced.
#[test]
fn words_of_war_does_not_damage_a_target_that_left_and_returned() {
    let Setup {
        mut scenario,
        words,
        drawer,
        library_top,
    } = setup("Words of War", WORDS_OF_WAR, 1);
    let bear = scenario.add_creature(P0, "Blink Target", 2, 3).id();
    let blink = scenario
        .add_artifact_from_oracle(
            P0,
            "Blink Probe",
            "{T}: Exile target creature you control, then return it to the battlefield under its owner's control.",
        )
        .id();
    let mut runner = scenario.build();
    let announced_incarnation = runner.state().objects[&bear].incarnation;
    let index = first_ability_index(&runner, words);
    runner.activate(words, index).target_object(bear).resolve();
    resolve_stack(&mut runner);
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "reach-guard: Words installed its replacement before the blink"
    );

    let index = first_ability_index(&runner, blink);
    runner.activate(blink, index).target_object(bear).resolve();
    resolve_stack(&mut runner);
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
    assert_ne!(
        runner.state().objects[&bear].incarnation,
        announced_incarnation,
        "reach-guard: the production blink returned a new object"
    );

    draw_one(&mut runner, drawer);

    assert!(!in_hand(&runner, library_top), "the draw remains replaced");
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "the new incarnation must not receive the old target's damage"
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
            payment_mode: CastPaymentMode::Auto,
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

fn on_battlefield(runner: &GameRunner, object: ObjectId) -> bool {
    runner.state().objects[&object].zone == Zone::Battlefield
}

fn pending_return_choice(runner: &GameRunner) -> (engine::types::player::PlayerId, Vec<ObjectId>) {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => (*player, cards.clone()),
        other => panic!("expected a return choice, got {other:?}"),
    }
}

/// CR 101.4: every player announces their return in APNAP order BEFORE any
/// chosen permanent moves. While a later player is still choosing, the earlier
/// players' picks must still be on the battlefield — across a multi-candidate
/// prompt (P0), a sole-candidate seat (P1, no prompt) and a seat with nothing
/// to return (P2 has no permanents; P3 prompts after it).
#[test]
fn words_of_wind_collects_every_choice_before_any_permanent_moves() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let library_top = scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Wind", WORDS_OF_WIND)
        .id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Test Drawing Artifact", DRAW_A_CARD)
        .id();
    let p1_sole = scenario.add_creature(P1, "P1 Sole", 2, 2).id();
    let p3 = engine::types::player::PlayerId(3);
    let p3_a = scenario.add_creature(p3, "P3 A", 1, 1).id();
    let p3_b = scenario.add_creature(p3, "P3 B", 1, 1).id();
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    draw_one(&mut runner, drawer);

    let (chooser, cards) = pending_return_choice(&runner);
    assert_eq!(chooser, P0, "the active player announces first");
    assert!(cards.contains(&words) && cards.contains(&drawer));
    runner
        .act(GameAction::SelectCards {
            cards: vec![drawer],
        })
        .expect("P0's choice is accepted");

    let (chooser, cards) = pending_return_choice(&runner);
    assert_eq!(
        chooser, p3,
        "P1 (sole candidate) and P2 (empty) are skipped"
    );
    assert_eq!(cards.len(), 2);
    assert!(
        on_battlefield(&runner, drawer),
        "P0's pick must not move while P3 is still choosing"
    );
    assert!(
        on_battlefield(&runner, p1_sole),
        "P1's forced pick must not move while P3 is still choosing"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![p3_b] })
        .expect("P3's choice is accepted");
    resolve_stack(&mut runner);

    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert!(in_hand(&runner, drawer));
    assert!(in_hand(&runner, p1_sole));
    assert!(in_hand(&runner, p3_b));
    assert!(on_battlefield(&runner, words));
    assert!(on_battlefield(&runner, p3_a), "unchosen permanents stay");
}

/// CR 101.4: with two prompting players, the first player's chosen permanent
/// stays on the battlefield while the second player chooses, then both move.
#[test]
fn words_of_wind_two_prompting_players_choose_before_either_moves() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Wind", WORDS_OF_WIND)
        .id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Test Drawing Artifact", DRAW_A_CARD)
        .id();
    let p1_a = scenario.add_creature(P1, "P1 A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 B", 1, 1).id();
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    draw_one(&mut runner, drawer);

    runner
        .act(GameAction::SelectCards { cards: vec![words] })
        .expect("P0's choice is accepted");
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(chooser, P1);
    assert!(
        on_battlefield(&runner, words),
        "P0's chosen Words stays on the battlefield during P1's prompt"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![p1_a] })
        .expect("P1's choice is accepted");
    resolve_stack(&mut runner);
    assert!(in_hand(&runner, words) && in_hand(&runner, p1_a));
    assert!(on_battlefield(&runner, p1_b));
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

/// CR 800.4a + CR 608.2m + CR 101.4: the active player concedes while choosing
/// their Words of Wind return. The resolution continues fully for the players
/// still in the game: the departed player returns nothing, and P1–P3 still
/// choose in turn order and return their picks together.
///
/// REVERT-FAILING: without the departure reconciliation, the departed
/// player's prompt is left standing (or is replaced by unrelated priority) and
/// no surviving player returns anything.
#[test]
fn words_of_wind_survivors_still_choose_after_the_chooser_concedes() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Wind", WORDS_OF_WIND)
        .id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Test Drawing Artifact", DRAW_A_CARD)
        .id();
    let p1_a = scenario.add_creature(P1, "P1 A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 B", 1, 1).id();
    let p2 = engine::types::player::PlayerId(2);
    let p2_sole = scenario.add_creature(p2, "P2 Sole", 1, 1).id();
    let p3 = engine::types::player::PlayerId(3);
    let p3_a = scenario.add_creature(p3, "P3 A", 1, 1).id();
    let p3_b = scenario.add_creature(p3, "P3 B", 1, 1).id();
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    draw_one(&mut runner, drawer);

    let (chooser, cards) = pending_return_choice(&runner);
    assert_eq!(
        chooser, P0,
        "reach-guard: the active player announces first"
    );
    assert!(cards.contains(&words) && cards.contains(&drawer));

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes during their return choice");

    let (chooser, cards) = pending_return_choice(&runner);
    assert_eq!(
        chooser, P1,
        "the walk continues with the next living player"
    );
    assert_eq!(cards.len(), 2);
    runner
        .act(GameAction::SelectCards { cards: vec![p1_a] })
        .expect("P1's choice is accepted");

    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(
        chooser, p3,
        "P2's sole permanent is forced; P3 chooses next"
    );
    assert!(
        on_battlefield(&runner, p1_a) && on_battlefield(&runner, p2_sole),
        "earlier picks stay on the battlefield while P3 chooses"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![p3_b] })
        .expect("P3's choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_hand(&runner, p1_a));
    assert!(in_hand(&runner, p2_sole));
    assert!(in_hand(&runner, p3_b));
    assert!(on_battlefield(&runner, p1_b), "unchosen permanents stay");
    assert!(on_battlefield(&runner, p3_a), "unchosen permanents stay");
}

/// Four players with two permanents each (Words of Wind and its drawer for P0),
/// Wind activated and its replaced draw taken: P0 is prompted for the return.
struct WindTable {
    runner: GameRunner,
    drawer: ObjectId,
    /// Each of P1..P3's two creatures, in seat order.
    seats: [[ObjectId; 2]; 3],
}

fn wind_table_at_p0_choice() -> WindTable {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Wind", WORDS_OF_WIND)
        .id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Test Drawing Artifact", DRAW_A_CARD)
        .id();
    let mut seats = [[ObjectId(0); 2]; 3];
    for (slot, seat) in seats.iter_mut().enumerate() {
        let player = engine::types::player::PlayerId(slot as u8 + 1);
        for (index, id) in seat.iter_mut().enumerate() {
            *id = scenario
                .add_creature(player, &format!("Seat {} Creature {index}", slot + 1), 1, 1)
                .id();
        }
    }
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    draw_one(&mut runner, drawer);
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(
        chooser, P0,
        "reach-guard: the active player announces first"
    );
    WindTable {
        runner,
        drawer,
        seats,
    }
}

/// CR 800.4a + CR 608.2m + CR 101.4: P0 and P1 have already chosen when P2, the
/// current chooser, concedes. The earlier picks stay collected, P3 still
/// chooses, and all surviving picks return together; the carrier is cleared and
/// the resolution finishes at priority.
///
/// REVERT-FAILING: with no departure reconciliation the carrier (and P0/P1's
/// picks and P3's turn) is stranded behind ordinary priority.
#[test]
fn words_of_wind_later_chooser_concedes_after_earlier_picks() {
    let WindTable {
        mut runner,
        drawer,
        seats: [p1, p2, p3],
    } = wind_table_at_p0_choice();

    runner
        .act(GameAction::SelectCards {
            cards: vec![drawer],
        })
        .expect("P0's choice is accepted");
    runner
        .act(GameAction::SelectCards { cards: vec![p1[0]] })
        .expect("P1's choice is accepted");
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(chooser, engine::types::player::PlayerId(2));

    runner
        .act(GameAction::Concede {
            player_id: engine::types::player::PlayerId(2),
        })
        .expect("P2 concedes during their return choice");

    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(
        chooser,
        engine::types::player::PlayerId(3),
        "the walk continues with the next living player"
    );
    assert!(
        on_battlefield(&runner, drawer) && on_battlefield(&runner, p1[0]),
        "earlier picks stay on the battlefield until the batch is delivered"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![p3[1]] })
        .expect("P3's choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_hand(&runner, drawer), "P0's earlier pick returns");
    assert!(in_hand(&runner, p1[0]), "P1's earlier pick returns");
    assert!(in_hand(&runner, p3[1]), "the later chooser still returns");
    assert!(on_battlefield(&runner, p1[1]) && on_battlefield(&runner, p3[0]));
    for id in p2 {
        assert!(
            !on_battlefield(&runner, id),
            "the departed seat's objects left"
        );
        assert!(!in_hand(&runner, id), "the departed seat returns nothing");
    }
    assert!(
        runner.state().pending_player_scope_choice.is_none(),
        "the carrier is cleared once the instruction completes"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 800.4a + CR 101.4: a player who has not yet been asked (P3) concedes while
/// P0 is choosing. Nothing is stranded: P0 still holds the open prompt, P1 and
/// P2 still choose, P3 is never prompted, and the batch returns the survivors'
/// picks together.
///
/// REVERT-FAILING: the departed seat's name stays in the carrier's remaining
/// players and is prompted (or its stale pick delivered) after it has left.
#[test]
fn words_of_wind_noncurrent_remaining_chooser_concedes() {
    let WindTable {
        mut runner,
        drawer,
        seats: [p1, p2, p3],
    } = wind_table_at_p0_choice();

    let p3_seat = engine::types::player::PlayerId(3);
    let queued = |runner: &GameRunner| {
        runner
            .state()
            .pending_player_scope_choice
            .as_ref()
            .expect("the return carrier is open while P0 chooses")
            .remaining_players
            .clone()
    };
    assert!(
        queued(&runner).contains(&p3_seat),
        "reach-guard: P3 waits in the parked chooser queue before conceding"
    );

    runner
        .act(GameAction::Concede {
            player_id: engine::types::player::PlayerId(3),
        })
        .expect("P3 concedes while P0 is choosing");
    // The carrier boundary, not the end state: with the prune reverted P3
    // would stay queued (and merely find an empty pool later).
    assert!(
        !queued(&runner).contains(&p3_seat),
        "the departed seat leaves the parked chooser queue at once"
    );
    let (chooser, cards) = pending_return_choice(&runner);
    assert_eq!(chooser, P0, "P0's open prompt still stands");
    assert!(cards.contains(&drawer));

    runner
        .act(GameAction::SelectCards {
            cards: vec![drawer],
        })
        .expect("P0's choice is accepted");
    runner
        .act(GameAction::SelectCards { cards: vec![p1[1]] })
        .expect("P1's choice is accepted");
    runner
        .act(GameAction::SelectCards { cards: vec![p2[0]] })
        .expect("P2's choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_hand(&runner, drawer) && in_hand(&runner, p1[1]) && in_hand(&runner, p2[0]));
    assert!(on_battlefield(&runner, p1[0]) && on_battlefield(&runner, p2[1]));
    for id in p3 {
        assert!(!in_hand(&runner, id), "the departed seat returns nothing");
    }
    assert!(
        runner.state().pending_player_scope_choice.is_none(),
        "the carrier is cleared once the instruction completes"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 614.11a + CR 121.6b: Words of Wind replaces the first draw of "draw two
/// cards"; the replacement, including every player's return choice, completes
/// before the sequence resumes with the second draw.
#[test]
fn words_of_wind_inside_a_two_card_draw_resumes_the_second_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library A", "P1 Library B"]);
    let library_top = scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Wind", WORDS_OF_WIND)
        .id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Test Draw Two Artifact", "{T}: Draw two cards.")
        .id();
    let p1_a = scenario.add_creature(P1, "P1 A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 B", 1, 1).id();
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    let index = first_ability_index(&runner, drawer);
    runner.activate(drawer, index).resolve();
    resolve_stack(&mut runner);

    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(chooser, P0);
    assert!(
        !in_hand(&runner, library_top),
        "the second draw waits for the replacement to finish"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![drawer],
        })
        .expect("P0's choice is accepted");
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(chooser, P1);
    assert!(
        !in_hand(&runner, library_top),
        "the second draw still waits while P1 chooses"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![p1_a] })
        .expect("P1's choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_hand(&runner, drawer) && in_hand(&runner, p1_a));
    assert!(on_battlefield(&runner, words) && on_battlefield(&runner, p1_b));
    assert!(
        in_hand(&runner, library_top),
        "the sequence resumes: the second draw happens"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 2,
        "one drawn card plus the returned artifact"
    );
}

/// CR 118.12 + CR 101.4: "Each player returns a permanent they control to its
/// owner's hand unless they pay 2 life" (Tradewind Rider Avatar) keeps its
/// unless-cost: a player is offered the payment before anything returns. The
/// collect-then-move route does not take an instruction with a per-player
/// rider.
///
/// REVERT-FAILING: if the collector takes the instruction, the unless-cost is
/// dropped and the first player is asked only which permanent to return.
#[test]
fn each_player_return_unless_they_pay_life_keeps_the_payment_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let avatar = scenario
        .add_artifact_from_oracle(P0, "Tradewind Rider Avatar", TRADEWIND_RIDER_AVATAR)
        .id();
    let p0_extra = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_a = scenario.add_creature(P1, "P1 A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 B", 1, 1).id();
    scenario.with_mana_pool(P0, colorless_mana(3));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, avatar);
    runner.activate(avatar, index).resolve();
    resolve_stack(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "a player must be offered the 2-life payment, got {:?}",
        runner.state().waiting_for
    );
    for id in [avatar, p0_extra, p1_a, p1_b] {
        assert!(
            on_battlefield(&runner, id),
            "nothing returns before the payment is answered"
        );
    }
}

/// CR 101.4 + CR 608.2n: Curfew ("Each player returns a creature they control
/// to its owner's hand") collects every player's choice at spell resolution,
/// returns the picks together, and then goes to its owner's graveyard.
#[test]
fn curfew_collects_choices_then_returns_together_and_finishes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let curfew = scenario
        .add_spell_to_hand_from_oracle(P0, "Curfew", true, CURFEW)
        .id();
    let p0_a = scenario.add_creature(P0, "P0 A", 1, 1).id();
    let p0_b = scenario.add_creature(P0, "P0 B", 1, 1).id();
    let p1_a = scenario.add_creature(P1, "P1 A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 B", 1, 1).id();
    let mut runner = scenario.build();

    runner.cast(curfew).resolve();
    let (chooser, cards) = pending_return_choice(&runner);
    assert_eq!(chooser, P0);
    assert_eq!(cards.len(), 2);
    runner
        .act(GameAction::SelectCards { cards: vec![p0_a] })
        .expect("P0's choice is accepted");
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(chooser, P1);
    assert!(
        on_battlefield(&runner, p0_a),
        "P0's pick does not move while P1 chooses"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![p1_b] })
        .expect("P1's choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_hand(&runner, p0_a) && in_hand(&runner, p1_b));
    assert!(on_battlefield(&runner, p0_b) && on_battlefield(&runner, p1_a));
    assert_eq!(runner.state().objects[&curfew].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 903.9b + CR 616.1 + CR 101.4: a commander among the collected returns may
/// go to the command zone instead. Its owner's choice pauses the one
/// simultaneous return after every player has chosen; answering it finishes the
/// return and the spell.
#[test]
fn curfew_commander_replacement_pauses_the_batch_and_finishes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let curfew = scenario
        .add_spell_to_hand_from_oracle(P0, "Curfew", true, CURFEW)
        .id();
    let p0_a = scenario.add_creature(P0, "P0 A", 1, 1).id();
    let p0_b = scenario.add_creature(P0, "P0 B", 1, 1).id();
    // CR 903.3: the commander designation is an attribute of the card, so the
    // commander stays on the battlefield (`with_commander` would move it).
    let commander = {
        let mut builder = scenario.add_creature(P1, "Commanding Bear", 2, 2);
        builder.as_legendary().commander();
        builder.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    runner.cast(curfew).resolve();
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(chooser, P0);
    runner
        .act(GameAction::SelectCards { cards: vec![p0_a] })
        .expect("P0's choice is accepted");

    // Reach guard: P1's only creature is forced, and its owner's CR 903.9b
    // replacement choice pauses the batch.
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected P1's CR 903.9b replacement choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 sends the commander to the command zone");
    resolve_stack(&mut runner);

    assert_eq!(runner.state().objects[&commander].zone, Zone::Command);
    assert!(in_hand(&runner, p0_a));
    assert!(on_battlefield(&runner, p0_b));
    assert_eq!(runner.state().objects[&curfew].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
}

fn pending_discard_prompt(runner: &GameRunner) -> (PlayerId, Vec<ObjectId>) {
    match &runner.state().waiting_for {
        WaitingFor::DiscardChoice { player, cards, .. } => (*player, cards.clone()),
        other => panic!("expected a discard choice, got {other:?}"),
    }
}

/// `N + 1` players: P0 controls Words of Waste and Archivist ("{T}: Draw a
/// card.", a 1/1 that has been under P0's control since the turn began); each
/// opponent P1..PN holds its `hands` count of cards, and `extra` adds any
/// further permanents. The Waste draw replacement is armed and P0's draw is
/// taken, so the first opponent's discard choice is open.
fn waste_table_with<const N: usize>(
    hands: [usize; N],
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, [Vec<ObjectId>; N]) {
    let mut scenario = GameScenario::new_n_player(N as u8 + 1, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let library_top = scenario.add_card_to_library_top(P0, "P0 Library Top");
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Waste", WORDS_OF_WASTE)
        .id();
    let drawer = scenario
        .add_creature_from_oracle(P0, "Archivist", 1, 1, DRAW_A_CARD)
        .id();
    extra(&mut scenario);
    let mut hand_ids: [Vec<ObjectId>; N] = std::array::from_fn(|_| Vec::new());
    for (slot, count) in hands.into_iter().enumerate() {
        let player = PlayerId(slot as u8 + 1);
        for index in 0..count {
            hand_ids[slot].push(
                scenario
                    .add_creature_to_hand(player, &format!("P{} Hand {index}", slot + 1), 1, 1)
                    .id(),
            );
        }
    }
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    draw_one(&mut runner, drawer);
    (runner, library_top, hand_ids)
}

/// Three players: P1 and P2 each hold their `hands` count of cards.
fn waste_table(hands: [usize; 2]) -> (GameRunner, ObjectId, [Vec<ObjectId>; 2]) {
    waste_table_with(hands, |_| {})
}

fn in_graveyard(runner: &GameRunner, object: ObjectId) -> bool {
    runner.state().objects[&object].zone == Zone::Graveyard
}

/// CR 101.4 + CR 101.4a + CR 701.9a: "each opponent discards a card" — both
/// opponents choose before either card leaves a hand. While P2 is choosing,
/// P1's pick is still in P1's hand (so P2 can't learn it), no viewer is sent
/// the held pick, and after P2 answers both cards are discarded together and
/// the draw stays replaced.
///
/// REVERT-FAILING: with the per-seat route, P1's pick is already in the
/// graveyard when P2's prompt opens.
#[test]
fn words_of_waste_collects_both_opponents_choices_before_either_discards() {
    let (mut runner, library_top, [p1, p2]) = waste_table([2, 2]);

    let (chooser, cards) = pending_discard_prompt(&runner);
    assert_eq!(
        chooser, P1,
        "the first opponent in turn order chooses first"
    );
    assert!(cards.iter().all(|card| p1.contains(card)));
    let p1_pick = p1[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_pick],
        })
        .expect("P1's choice is accepted");

    let (chooser, cards) = pending_discard_prompt(&runner);
    assert_eq!(chooser, PlayerId(2));
    assert!(cards.iter().all(|card| p2.contains(card)));
    assert!(
        in_hand(&runner, p1_pick),
        "P1's chosen card stays in hand until P2 has chosen"
    );
    assert!(!in_graveyard(&runner, p1_pick));
    for viewer in [P0, P1, PlayerId(2)] {
        let view = engine::game::visibility::filter_state_for_viewer(runner.state(), viewer);
        assert!(
            view.pending_player_scope_choice
                .as_ref()
                .is_some_and(|carrier| carrier.selections.is_empty()),
            "viewer {viewer:?} must not be sent the held pick"
        );
    }

    let p2_pick = p2[1];
    runner
        .act(GameAction::SelectCards {
            cards: vec![p2_pick],
        })
        .expect("P2's choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_graveyard(&runner, p1_pick) && in_graveyard(&runner, p2_pick));
    assert!(in_hand(&runner, p1[1]) && in_hand(&runner, p2[0]));
    assert!(!in_hand(&runner, library_top), "the draw is replaced");
    assert!(runner.state().pending_player_scope_choice.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}

/// CR 101.4: a seat with nothing to choose (one card, so the discard is forced)
/// is also held back: its card leaves the hand with the batch, not before the
/// later chooser has chosen.
#[test]
fn words_of_waste_forced_discard_waits_for_the_later_chooser() {
    let (mut runner, _, [p1, p2]) = waste_table([1, 2]);

    let (chooser, _) = pending_discard_prompt(&runner);
    assert_eq!(
        chooser,
        PlayerId(2),
        "P1's only card needs no choice, so P2 is the first prompt"
    );
    assert!(
        in_hand(&runner, p1[0]),
        "the forced discard has not happened while P2 chooses"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![p2[0]] })
        .expect("P2's choice is accepted");
    resolve_stack(&mut runner);
    assert!(in_graveyard(&runner, p1[0]) && in_graveyard(&runner, p2[0]));
}

/// CR 800.4a + CR 101.4: P2 concedes while choosing. P1's earlier pick is still
/// discarded (it was chosen before the departure) and nothing is stranded.
#[test]
fn words_of_waste_chooser_concedes_and_the_earlier_pick_still_discards() {
    let (mut runner, library_top, [p1, _]) = waste_table([2, 2]);
    let p1_pick = p1[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_pick],
        })
        .expect("P1's choice is accepted");
    let (chooser, _) = pending_discard_prompt(&runner);
    assert_eq!(chooser, PlayerId(2));

    runner
        .act(GameAction::Concede {
            player_id: PlayerId(2),
        })
        .expect("P2 concedes during their discard choice");
    resolve_stack(&mut runner);

    assert!(in_graveyard(&runner, p1_pick), "P1's earlier pick discards");
    assert!(!in_hand(&runner, library_top), "the draw stays replaced");
    assert!(runner.state().pending_player_scope_choice.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 104.3a + CR 800.4a + CR 101.4 + CR 701.9a: four players. P1 has chosen,
/// and P3 (not yet asked) concedes while P2's prompt is open. P1's collected
/// pick, still in P1's hand, stays collected across the departure; once P2
/// answers, both survivors' picks are discarded together and P3 is never asked.
///
/// REVERT-FAILING: with the return-carrier reconciler pruning the shared
/// carrier, P1's in-hand pick is dropped as "not on the battlefield" and stays
/// in hand after P2 answers.
#[test]
fn words_of_waste_noncurrent_chooser_concedes_and_the_earlier_pick_still_discards() {
    let (mut runner, library_top, [p1, p2, _]) = waste_table_with([2, 2, 2], |_| {});
    let p3 = PlayerId(3);
    let p1_pick = p1[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_pick],
        })
        .expect("P1's choice is accepted");
    let (chooser, _) = pending_discard_prompt(&runner);
    assert_eq!(chooser, PlayerId(2), "reach-guard: P2 is choosing");
    assert!(
        runner
            .state()
            .pending_player_scope_choice
            .as_ref()
            .is_some_and(|carrier| carrier.remaining_players.contains(&p3)),
        "reach-guard: P3 waits in the parked chooser queue before conceding"
    );

    runner
        .act(GameAction::Concede { player_id: p3 })
        .expect("P3 concedes while P2 is choosing");

    let (chooser, _) = pending_discard_prompt(&runner);
    assert_eq!(chooser, PlayerId(2), "P2's open prompt still stands");
    let carrier = runner
        .state()
        .pending_player_scope_choice
        .as_ref()
        .expect("the discard carrier is still open while P2 chooses");
    assert!(
        !carrier.remaining_players.contains(&p3),
        "the departed seat leaves the parked chooser queue"
    );
    assert_eq!(
        carrier.selections.len(),
        1,
        "P1's collected pick survives P3's departure"
    );
    assert!(in_hand(&runner, p1_pick), "the pick is not yet discarded");

    let p2_pick = p2[1];
    runner
        .act(GameAction::SelectCards {
            cards: vec![p2_pick],
        })
        .expect("P2's choice is accepted");
    resolve_stack(&mut runner);

    assert!(
        in_graveyard(&runner, p1_pick),
        "P1's collected pick discards"
    );
    assert!(in_graveyard(&runner, p2_pick), "P2's pick discards");
    assert!(in_hand(&runner, p1[1]) && in_hand(&runner, p2[0]));
    assert!(!in_hand(&runner, library_top), "the draw stays replaced");
    assert!(runner.state().pending_player_scope_choice.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}

const LIBRARY_OF_LENG: &str = "You have no maximum hand size.\nIf an effect causes you to discard a card, discard it, but you may put it on top of your library instead of into your graveyard.";

/// CR 101.4 + CR 701.9a + CR 614.11a: P1 controls Library of Leng. Both
/// opponents choose, then the collected discards are delivered; P1's discard
/// pauses on Leng's optional replacement. Answering it with the option whose
/// description is `answer` (Leng's replacement text, or "Decline") finishes
/// the delivery and the replaced draw before priority. Returns the runner and
/// P1's pick.
fn waste_discard_paused_by_library_of_leng(answer: &str) -> (GameRunner, ObjectId) {
    let (mut runner, library_top, [p1, p2]) = waste_table_with([2, 2], |scenario| {
        scenario.add_artifact_from_oracle(P1, "Library of Leng", LIBRARY_OF_LENG);
    });
    let (p1_pick, p2_pick) = (p1[0], p2[1]);
    let (chooser, _) = pending_discard_prompt(&runner);
    assert_eq!(chooser, P1, "reach-guard: P1 chooses first");
    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_pick],
        })
        .expect("P1's choice is accepted");
    let (chooser, _) = pending_discard_prompt(&runner);
    assert_eq!(chooser, PlayerId(2));
    runner
        .act(GameAction::SelectCards {
            cards: vec![p2_pick],
        })
        .expect("P2's choice is accepted");

    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the collected discard must pause on Library of Leng, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "Leng's controller makes the choice");
    let index = candidates
        .iter()
        .position(|candidate| candidate.description == answer)
        .unwrap_or_else(|| panic!("no {answer:?} option among {candidates:?}"));
    assert!(
        in_hand(&runner, p1_pick),
        "P1's pick has not moved while the replacement choice is open"
    );
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the replacement choice is accepted");
    resolve_stack(&mut runner);

    assert!(in_graveyard(&runner, p2_pick), "P2's pick discards");
    assert!(in_hand(&runner, p1[1]) && in_hand(&runner, p2[0]));
    assert!(
        !in_hand(&runner, library_top),
        "the draw stays replaced: P0 draws nothing"
    );
    assert!(
        runner.state().pending_player_scope_choice.is_none(),
        "no carrier is stranded once the replaced draw finishes"
    );
    assert!(runner.state().pending_discard_batch.is_none());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    (runner, p1_pick)
}

/// Accepting Leng puts P1's discarded card on top of P1's library.
///
/// REVERT-FAILING (this and the declining sibling): if the delivery's
/// replacement pause is reported as a completed discard, the parent draw's
/// completion hands P0 priority over Leng's prompt, so P1 is never asked.
#[test]
fn words_of_waste_discard_paused_by_library_of_leng_accepted_goes_to_library_top() {
    let leng_replacement = LIBRARY_OF_LENG.lines().last().expect("Leng has two lines");
    let (runner, p1_pick) = waste_discard_paused_by_library_of_leng(leng_replacement);
    assert_eq!(runner.state().objects[&p1_pick].zone, Zone::Library);
    assert_eq!(
        runner.state().players[1].library.front(),
        Some(&p1_pick),
        "the accepted card is on top of P1's library"
    );
}

/// Declining Leng puts P1's discarded card into P1's graveyard.
#[test]
fn words_of_waste_discard_paused_by_library_of_leng_declined_goes_to_graveyard() {
    let (runner, p1_pick) = waste_discard_paused_by_library_of_leng("Decline");
    assert!(in_graveyard(&runner, p1_pick));
}

/// CR 504.1 + CR 805.4b + CR 614.11a: Words of Wind replaces the active
/// player's draw-step draw in Two-Headed Giant. Once every player's return
/// choice is made, the active player has drawn nothing, the teammate still
/// takes their own draw-step draw, the team draw-step queue is empty, and the
/// active player receives priority in the draw step.
///
/// REVERT-FAILING: with the queue left parked at the active player, the
/// teammate never draws before priority.
#[test]
fn words_of_wind_in_the_two_headed_giant_draw_step_still_draws_the_teammate() {
    let mut scenario = GameScenario::new_with_format(
        engine::types::format::FormatConfig::two_headed_giant(),
        4,
        7,
    );
    scenario.at_phase(Phase::Upkeep);
    let p0_top = scenario.add_card_to_library_top(P0, "P0 Library Top");
    let p1_top = scenario.add_card_to_library_top(P1, "P1 Library Top");
    for player in [PlayerId(2), PlayerId(3)] {
        scenario.add_card_to_library_top(player, "Padding");
    }
    let words = scenario
        .add_enchantment_from_oracle(P0, "Words of Wind", WORDS_OF_WIND)
        .id();
    scenario.add_artifact_from_oracle(P0, "Second Permanent", DRAW_A_CARD);
    for player in [P1, PlayerId(2), PlayerId(3)] {
        for index in 0..2 {
            scenario.add_creature(player, &format!("P{} Creature {index}", player.0), 1, 1);
        }
    }
    scenario.with_mana_pool(P0, colorless_mana(1));
    let mut runner = scenario.build();

    let index = first_ability_index(&runner, words);
    runner.activate(words, index).resolve();
    resolve_stack(&mut runner);
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes toward the draw step");
    }
    let (chooser, _) = pending_return_choice(&runner);
    assert_eq!(
        chooser, P0,
        "reach-guard: Wind replaced P0's draw-step draw"
    );
    assert_eq!(runner.state().phase, Phase::Draw);
    assert_eq!(
        runner.state().pending_team_draw_step.first(),
        Some(&P0),
        "reach-guard: the active player's draw is parked at the queue front"
    );

    answer_choices_with_first_legal(&mut runner);

    assert!(
        !in_hand(&runner, p0_top),
        "the active player's draw is replaced"
    );
    assert!(in_hand(&runner, p1_top), "the teammate still draws");
    assert!(
        runner.state().pending_team_draw_step.is_empty(),
        "the draw-step queue is drained before priority"
    );
    assert_eq!(runner.state().phase, Phase::Draw);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}
