//! CR 612: word-substitution text changes driven through the real cast pipeline with the cards' verbatim Oracle text from the shared card database.

use std::collections::BTreeSet;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::combat::can_block_pair;
use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::perf_counters;
use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::text_substitution::{
    active_text_substitutions, classified_word_occurrences, restamp_resolving_spell_text,
    unclassified_word_positions,
};
use engine::types::ability::{
    ChoiceType, ContinuousModification, TextSubstitution, TextSubstitutionSpec, TextWordDomain,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Precombat main with stocked libraries so no player loses to an empty draw.
fn new_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island", "Island", "Island", "Island"]);
    }
    scenario
}

fn give(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, ObjectId(0), false, vec![]));
    }
}

fn build(scenario: GameScenario, db: &engine::database::CardDatabase) -> GameRunner {
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    evaluate_layers(runner.state_mut());
    runner
}

/// The mana types the engine offers for tapping `source` right now.
fn offered_mana(runner: &GameRunner, source: ObjectId) -> BTreeSet<ManaType> {
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    grouped
        .get(&source)
        .into_iter()
        .flatten()
        .filter_map(|action| match action {
            GameAction::TapLandForMana { selection }
            | GameAction::ActivateManaSource { selection } => Some(selection.mana_type),
            _ => None,
        })
        .collect()
}

fn walks(runner: &GameRunner, id: ObjectId) -> Vec<String> {
    runner.state().objects[&id]
        .keywords
        .iter()
        .filter_map(|k| match k {
            Keyword::Landwalk(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Casts a one-target text-changing instant at `target`, answering its word prompt.
fn cast_text_change(
    runner: &mut GameRunner,
    card: ObjectId,
    target: ObjectId,
    label: &str,
    mana: &[ManaType],
) {
    give(runner, runner.state().active_player, mana);
    runner
        .cast(card)
        .target_objects(&[target])
        .choose_option(label)
        .resolve();
}

fn pass_to_next_turn(runner: &mut GameRunner) {
    let start = runner.state().turn_number;
    for _ in 0..300 {
        if runner.state().turn_number > start {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            other => panic!("turn advance stalled at {other:?}"),
        }
    }
    panic!("turn did not advance");
}

macro_rules! db {
    () => {
        match load_db() {
            Some(db) => db,
            None => {
                eprintln!("skipping: integration card fixture not available");
                return;
            }
        }
    };
}

/// A land-type word in a keyword is text (CR 612.1), and a change with no stated duration lasts indefinitely (CR 611.2a).
#[test]
fn magical_hack_changes_landwalk_indefinitely() {
    let db = db!();
    let mut scenario = new_scenario();
    let hack = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let untouched = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let _plains = scenario.add_real_card(P1, "Plains", Zone::Battlefield, db);
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = build(scenario, db);

    assert_eq!(
        walks(&runner, wraith),
        ["Swamp"],
        "baseline: printed Swampwalk"
    );
    assert!(
        can_block_pair(runner.state(), blocker, wraith),
        "baseline: the defender controls a Plains, not a Swamp"
    );
    cast_text_change(
        &mut runner,
        hack,
        wraith,
        "Swamp -> Plains",
        &[ManaType::Blue],
    );

    assert_eq!(walks(&runner, wraith), ["Plains"]);
    assert_eq!(
        walks(&runner, untouched),
        ["Swamp"],
        "the other Wraith is unchanged"
    );
    assert!(
        !can_block_pair(runner.state(), blocker, wraith),
        "CR 702.14c: it now has plainswalk and the defender controls a Plains"
    );
    assert!(
        can_block_pair(runner.state(), blocker, untouched),
        "guard: the untouched Wraith is still blockable"
    );

    pass_to_next_turn(&mut runner);
    assert_eq!(
        walks(&runner, wraith),
        ["Plains"],
        "CR 611.2a: no stated duration lasts until end of game"
    );
}

/// An indefinite text change ends when the permanent leaves the battlefield, so the returned object has its printed text (CR 400.7).
#[test]
fn indefinite_text_change_ends_when_the_permanent_leaves_and_returns() {
    let db = db!();
    let mut scenario = new_scenario();
    let hack = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);

    cast_text_change(
        &mut runner,
        hack,
        wraith,
        "Swamp -> Plains",
        &[ManaType::Blue],
    );
    assert_eq!(
        walks(&runner, wraith),
        ["Plains"],
        "reach-guard: the change is live while the permanent stays on the battlefield"
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), wraith, Zone::Hand, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), wraith, Zone::Battlefield, &mut events);
    evaluate_layers(runner.state_mut());

    assert_eq!(runner.state().objects[&wraith].zone, Zone::Battlefield);
    assert_eq!(
        walks(&runner, wraith),
        ["Swamp"],
        "CR 400.7: the returned permanent is a new object with its printed Swampwalk"
    );
}

/// The type line's land-type word changes and the derived intrinsic mana ability follows (CR 305.6).
#[test]
fn magical_hack_changes_basic_land_subtype() {
    let db = db!();
    let mut scenario = new_scenario();
    let hack = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let swamp = scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    let mut runner = build(scenario, db);

    cast_text_change(
        &mut runner,
        hack,
        forest,
        "Forest -> Island",
        &[ManaType::Blue],
    );

    let obj = &runner.state().objects[&forest];
    assert_eq!(obj.card_types.subtypes, ["Island"]);
    assert_eq!(obj.name, "Forest", "CR 612.2: the name is not a word");
    assert_eq!(
        runner.state().objects[&swamp].card_types.subtypes,
        ["Swamp"],
        "the other basic keeps its type"
    );

    assert_eq!(
        offered_mana(&runner, forest),
        BTreeSet::from([ManaType::Blue]),
        "CR 305.6: taps for {{U}}, no longer {{G}}"
    );
    assert_eq!(
        offered_mana(&runner, swamp),
        BTreeSet::from([ManaType::Black]),
        "guard: an untouched basic still taps for its own color"
    );
}

/// CR 613.8: dependency order beats timestamp for a chain, in both casting orders.
#[test]
fn chain_of_text_changes_applies_dependency_order_in_both_casting_orders() {
    let db = db!();
    for reversed in [false, true] {
        let mut scenario = new_scenario();
        let first = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
        let second = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
        let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
        let mut runner = build(scenario, db);

        let (label_a, label_b) = ("Swamp -> Plains", "Plains -> Forest");
        let (one, two) = if reversed {
            (label_b, label_a)
        } else {
            (label_a, label_b)
        };
        cast_text_change(&mut runner, first, wraith, one, &[ManaType::Blue]);
        cast_text_change(&mut runner, second, wraith, two, &[ManaType::Blue]);

        assert_eq!(
            walks(&runner, wraith),
            ["Forest"],
            "CR 613.8a: the second change depends on the first (reversed: {reversed})"
        );
    }
}

/// A CR 613.8b loop falls back to timestamp order.
#[test]
fn loop_of_text_changes_applies_timestamp_order() {
    let db = db!();
    for (first_label, second_label, expected) in [
        ("Swamp -> Plains", "Swamp -> Forest", "Plains"),
        ("Swamp -> Forest", "Swamp -> Plains", "Forest"),
    ] {
        let mut scenario = new_scenario();
        let a = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
        let b = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
        let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
        let mut runner = build(scenario, db);
        cast_text_change(&mut runner, a, wraith, first_label, &[ManaType::Blue]);
        cast_text_change(&mut runner, b, wraith, second_label, &[ManaType::Blue]);
        assert_eq!(
            walks(&runner, wraith),
            [expected],
            "{first_label} then {second_label}"
        );
    }
}

/// A loop does not drag a chain effect on the same object back to timestamp order (CR 613.8b), and an unrelated object's same-`from` pair stays confined.
#[test]
fn chain_plus_loop_on_one_object_and_unrelated_loop_on_another() {
    let db = db!();
    let mut scenario = new_scenario();
    let hacks: Vec<ObjectId> = (0..5)
        .map(|_| scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db))
        .collect();
    let x = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let y = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);

    // Object X: Plains -> Forest, then Swamp -> Plains, then Swamp -> Forest.
    cast_text_change(
        &mut runner,
        hacks[0],
        x,
        "Plains -> Forest",
        &[ManaType::Blue],
    );
    cast_text_change(
        &mut runner,
        hacks[1],
        x,
        "Swamp -> Plains",
        &[ManaType::Blue],
    );
    cast_text_change(
        &mut runner,
        hacks[2],
        x,
        "Swamp -> Forest",
        &[ManaType::Blue],
    );
    // Object Y: a same-`from` pair on another object.
    cast_text_change(
        &mut runner,
        hacks[3],
        y,
        "Swamp -> Plains",
        &[ManaType::Blue],
    );
    cast_text_change(
        &mut runner,
        hacks[4],
        y,
        "Swamp -> Forest",
        &[ManaType::Blue],
    );

    assert_eq!(
        walks(&runner, x),
        ["Forest"],
        "CR 613.8b: B and C loop and B, the older, applies first; A depends on B and follows it"
    );
    assert_eq!(
        walks(&runner, y),
        ["Plains"],
        "Y's loop applies in timestamp order"
    );
}

/// Effects on two different objects never order against each other (CR 613.8a).
#[test]
fn same_from_changes_on_different_objects_do_not_interact() {
    let db = db!();
    let mut scenario = new_scenario();
    let a = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let b = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let y1 = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let y2 = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    cast_text_change(&mut runner, a, y1, "Swamp -> Plains", &[ManaType::Blue]);
    cast_text_change(&mut runner, b, y2, "Swamp -> Forest", &[ManaType::Blue]);
    assert_eq!(walks(&runner, y1), ["Plains"]);
    assert_eq!(walks(&runner, y2), ["Forest"]);
}

/// A color word inside a keyword is text, but a mana symbol is not (CR 612.2).
#[test]
fn color_word_in_keyword_changes_and_unrelated_color_is_a_no_op() {
    let db = db!();
    let mut scenario = new_scenario();
    let sleight_a = scenario.add_real_card(P0, "Sleight of Mind", Zone::Hand, db);
    let sleight_b = scenario.add_real_card(P0, "Sleight of Mind", Zone::Hand, db);
    let knight_a = scenario.add_real_card(P0, "Black Knight", Zone::Battlefield, db);
    let knight_b = scenario.add_real_card(P0, "Black Knight", Zone::Battlefield, db);
    let mut runner = build(scenario, db);

    let protection = |runner: &GameRunner, id: ObjectId| -> Vec<Keyword> {
        runner.state().objects[&id]
            .keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .cloned()
            .collect()
    };
    let baseline = protection(&runner, knight_a);
    assert_eq!(baseline.len(), 1, "reach-guard: the Knight has protection");

    cast_text_change(
        &mut runner,
        sleight_a,
        knight_a,
        "White -> Blue",
        &[ManaType::Blue],
    );
    assert_ne!(
        protection(&runner, knight_a),
        baseline,
        "protection from white became blue"
    );

    cast_text_change(
        &mut runner,
        sleight_b,
        knight_b,
        "Black -> Red",
        &[ManaType::Blue],
    );
    assert_eq!(
        protection(&runner, knight_b),
        baseline,
        "no white word: nothing changes"
    );
    assert_eq!(runner.state().objects[&knight_b].name, "Black Knight");
}

/// Printed static text changes, but an ability granted by another object's static is not text (CR 612.3).
#[test]
fn granted_abilities_are_not_text_but_printed_statics_are() {
    let db = db!();
    let walks_of = |runner: &GameRunner, id| walks(runner, id);

    // Hack on the Merfolk: the Lord's grant of islandwalk is not text of the Merfolk.
    let mut scenario = new_scenario();
    let hack = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let _lord = scenario.add_real_card(P0, "Lord of Atlantis", Zone::Battlefield, db);
    let merfolk = scenario.add_real_card(P0, "Merfolk of the Pearl Trident", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    assert_eq!(
        walks_of(&runner, merfolk),
        ["Island"],
        "reach-guard: the grant is live"
    );
    cast_text_change(
        &mut runner,
        hack,
        merfolk,
        "Island -> Forest",
        &[ManaType::Blue],
    );
    assert_eq!(
        walks_of(&runner, merfolk),
        ["Island"],
        "CR 612.3: granted, so not changed"
    );

    // Hack on the Lord: its printed static now grants forestwalk.
    let mut scenario = new_scenario();
    let hack = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let lord = scenario.add_real_card(P0, "Lord of Atlantis", Zone::Battlefield, db);
    let merfolk = scenario.add_real_card(P0, "Merfolk of the Pearl Trident", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    cast_text_change(
        &mut runner,
        hack,
        lord,
        "Island -> Forest",
        &[ManaType::Blue],
    );
    assert_eq!(
        walks_of(&runner, merfolk),
        ["Forest"],
        "the Lord's printed text changed"
    );
}

/// A text-changed static ability generates its effect from the changed text (CR 613.1c).
#[test]
fn changed_static_ability_applies_in_the_same_layer_pass() {
    let db = db!();
    let mut scenario = new_scenario();
    let sleight = scenario.add_real_card(P0, "Sleight of Mind", Zone::Hand, db);
    let moon = scenario.add_real_card(P0, "Bad Moon", Zone::Battlefield, db);
    let black = scenario
        .add_creature(P0, "Black Guy", 2, 2)
        .with_color(vec![ManaColor::Black])
        .id();
    let white = scenario
        .add_creature(P0, "White Guy", 2, 2)
        .with_color(vec![ManaColor::White])
        .id();
    let mut runner = build(scenario, db);

    let pt = |runner: &GameRunner, id: ObjectId| {
        let o = &runner.state().objects[&id];
        (o.power.unwrap(), o.toughness.unwrap())
    };
    assert_eq!(
        pt(&runner, black),
        (3, 3),
        "reach-guard: the anthem is live"
    );
    assert_eq!(pt(&runner, white), (2, 2));

    cast_text_change(
        &mut runner,
        sleight,
        moon,
        "Black -> White",
        &[ManaType::Blue],
    );
    assert_eq!(
        pt(&runner, white),
        (3, 3),
        "Bad Moon now reads \"White creatures\""
    );
    assert_eq!(pt(&runner, black), (2, 2));
    pass_to_next_turn(&mut runner);
    assert_eq!(pt(&runner, white), (3, 3), "indefinite");
}

/// The words in a mana ability's symbols are not text, but the color word in the animation is (CR 612.2).
#[test]
fn mana_symbols_are_not_words_but_the_color_word_is() {
    let db = db!();
    let mut scenario = new_scenario();
    let sleight = scenario.add_real_card(P0, "Sleight of Mind", Zone::Hand, db);
    let monument = scenario.add_real_card(P0, "Dromoka Monument", Zone::Battlefield, db);
    let mut runner = build(scenario, db);

    cast_text_change(
        &mut runner,
        sleight,
        monument,
        "Green -> Blue",
        &[ManaType::Blue],
    );
    // Animate it ({4}{G}{W}); the printed cost's symbols stay {G}{W}.
    give(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Green,
            ManaType::White,
        ],
    );
    runner.activate(monument, 1).resolve();
    let colors = runner.state().objects[&monument].color.clone();
    assert!(
        colors.contains(&ManaColor::Blue),
        "\"green\" became \"blue\": {colors:?}"
    );
    assert!(colors.contains(&ManaColor::White));
    assert!(!colors.contains(&ManaColor::Green));

    runner
        .act(GameAction::ActivateAbility {
            source_id: monument,
            ability_index: 0,
        })
        .expect("activate the mana ability");
    let WaitingFor::ChooseManaColor { choice, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the {{G}} or {{W}} prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        format!("{choice:?}").contains("[Green, White]"),
        "CR 612.2: {{G}} and {{W}} are mana symbols, not color words: {choice:?}"
    );
}

/// A text change on a spell on the stack is what resolves (CR 608.2b).
#[test]
fn text_change_on_a_spell_changes_what_resolves() {
    let db = db!();
    for changed in [false, true] {
        let mut scenario = new_scenario();
        let terror = scenario.add_real_card(P0, "Terror", Zone::Hand, db);
        let sleight = scenario.add_real_card(P1, "Sleight of Mind", Zone::Hand, db);
        let red = scenario
            .add_creature(P1, "Red Guy", 2, 2)
            .with_color(vec![ManaColor::Red])
            .id();
        let mut runner = build(scenario, db);
        give(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);
        give(&mut runner, P1, &[ManaType::Blue]);

        let mut committed = runner.cast(terror).target_objects(&[red]).commit();
        if changed {
            committed.act(GameAction::PassPriority).expect("P0 passes");
            committed
                .cast(sleight)
                .target_objects(&[terror])
                .choose_option("Black -> Red")
                .commit()
                .resolve();
        } else {
            committed.resolve();
        }
        let zone = runner.state().objects[&red].zone;
        if changed {
            assert_eq!(
                zone,
                Zone::Battlefield,
                "Terror now reads nonred: its target is illegal"
            );
        } else {
            assert_eq!(
                zone,
                Zone::Graveyard,
                "baseline: unchanged Terror kills the red creature"
            );
        }
    }
}

/// A resolving spell destroys the changed land type (CR 608.2b).
#[test]
fn text_change_on_acid_rain_changes_which_lands_die() {
    let db = db!();
    for changed in [false, true] {
        let mut scenario = new_scenario();
        let rain = scenario.add_real_card(P0, "Acid Rain", Zone::Hand, db);
        let hack = scenario.add_real_card(P1, "Magical Hack", Zone::Hand, db);
        let forest = scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
        let island = scenario.add_real_card(P1, "Island", Zone::Battlefield, db);
        let mut runner = build(scenario, db);
        give(
            &mut runner,
            P0,
            &[
                ManaType::Blue,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        give(&mut runner, P1, &[ManaType::Blue]);

        let mut committed = runner.cast(rain).commit();
        if changed {
            committed.act(GameAction::PassPriority).expect("P0 passes");
            committed
                .cast(hack)
                .target_objects(&[rain])
                .choose_option("Forest -> Island")
                .commit()
                .resolve();
        } else {
            committed.resolve();
        }
        let zone = |id| runner.state().objects[&id].zone;
        if changed {
            assert_eq!(
                zone(island),
                Zone::Graveyard,
                "Acid Rain reads \"Destroy all Islands\""
            );
            assert_eq!(zone(forest), Zone::Battlefield);
        } else {
            assert_eq!(zone(forest), Zone::Graveyard, "baseline: Forests die");
            assert_eq!(zone(island), Zone::Battlefield);
        }
    }
}

/// A text change on a permanent spell carries onto the permanent (CR 400.7a).
#[test]
fn text_change_on_a_permanent_spell_carries_to_the_permanent() {
    let db = db!();
    for changed in [false, true] {
        let mut scenario = new_scenario();
        let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Hand, db);
        let hack = scenario.add_real_card(P1, "Magical Hack", Zone::Hand, db);
        let mut runner = build(scenario, db);
        give(
            &mut runner,
            P0,
            &[
                ManaType::Black,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        give(&mut runner, P1, &[ManaType::Blue]);

        let mut committed = runner.cast(wraith).commit();
        if changed {
            committed.act(GameAction::PassPriority).expect("P0 passes");
            committed
                .cast(hack)
                .target_objects(&[wraith])
                .choose_option("Swamp -> Plains")
                .commit()
                .resolve();
        } else {
            committed.resolve();
        }
        assert_eq!(runner.state().objects[&wraith].zone, Zone::Battlefield);
        if changed {
            assert_eq!(walks(&runner, wraith), ["Plains"]);
            assert!(
                runner.state().transient_continuous_effects.iter().any(|t| t
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::SubstituteTextWord { .. }))),
                "the effect outlives the stack leg while the permanent lives"
            );
        } else {
            assert_eq!(walks(&runner, wraith), ["Swamp"], "baseline");
        }
    }
}

/// A text change on a non-permanent spell does not outlive it (CR 400.7a).
#[test]
fn text_change_on_a_non_permanent_spell_is_gone_when_it_leaves_the_stack() {
    let db = db!();
    let mut scenario = new_scenario();
    let terror = scenario.add_real_card(P0, "Terror", Zone::Hand, db);
    let sleight = scenario.add_real_card(P1, "Sleight of Mind", Zone::Hand, db);
    let green = scenario
        .add_creature(P1, "Green Guy", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let mut runner = build(scenario, db);
    give(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);
    give(&mut runner, P1, &[ManaType::Blue]);

    let mut committed = runner.cast(terror).target_objects(&[green]).commit();
    committed.act(GameAction::PassPriority).expect("P0 passes");
    let mut response = committed
        .cast(sleight)
        .target_objects(&[terror])
        .choose_option("Black -> Red")
        .commit();
    assert!(
        response
            .state()
            .transient_continuous_effects
            .iter()
            .all(|t| t.modifications.is_empty() || !has_text_word(t.modifications.as_slice())),
        "the effect is not installed until Sleight resolves"
    );
    let _ = &mut response;
    response.resolve();

    assert_eq!(
        runner.state().objects[&green].zone,
        Zone::Graveyard,
        "Terror still resolved"
    );
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|t| has_text_word(&t.modifications)),
        "the change on Terror left with Terror"
    );
}

fn has_text_word(mods: &[ContinuousModification]) -> bool {
    mods.iter()
        .any(|m| matches!(m, ContinuousModification::SubstituteTextWord { .. }))
}

/// The prompt lists exactly the domain's pairs, and Crystal Spray folds both domains and draws after the change (CR 608.2d).
#[test]
fn crystal_spray_prompts_both_domains_then_draws() {
    let db = db!();
    let mut scenario = new_scenario();
    let spray = scenario.add_real_card(P0, "Crystal Spray", Zone::Hand, db);
    let moon = scenario.add_real_card(P0, "Bad Moon", Zone::Battlefield, db);
    let black = scenario
        .add_creature(P0, "Black Guy", 2, 2)
        .with_color(vec![ManaColor::Black])
        .id();
    let red = scenario
        .add_creature(P0, "Red Guy", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = build(scenario, db);
    let pt = |runner: &GameRunner, id: ObjectId| {
        let o = &runner.state().objects[&id];
        (o.power.unwrap(), o.toughness.unwrap())
    };
    let hand_size = |runner: &GameRunner| runner.state().players[P0.0 as usize].hand.len();
    assert_eq!(
        pt(&runner, black),
        (3, 3),
        "reach-guard: the anthem is live"
    );
    assert_eq!(pt(&runner, red), (2, 2));
    give(
        &mut runner,
        P0,
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
    );

    let outcome = runner.cast(spray).target_objects(&[moon]).resolve();
    let WaitingFor::NamedChoice { choice_type, .. } = outcome.final_waiting_for().clone() else {
        panic!(
            "expected the word prompt, got {:?}",
            outcome.final_waiting_for()
        );
    };
    let ChoiceType::Labeled { options } = choice_type else {
        panic!("expected a labeled prompt");
    };
    assert_eq!(options.len(), 40, "20 color pairs + 20 land pairs");
    assert!(options.iter().all(|o| {
        let (a, b) = o.split_once(" -> ").unwrap();
        a != b
    }));
    assert!(options.contains(&"Black -> Blue".to_string()));
    assert!(options.contains(&"Forest -> Island".to_string()));

    runner
        .act(GameAction::ChooseOption {
            choice: "Black -> Red".into(),
        })
        .expect("answer");
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        runner.state().last_named_choice.is_none(),
        "the answer was consumed by the latch"
    );
    assert_eq!(
        pt(&runner, red),
        (3, 3),
        "Bad Moon now reads \"Red creatures\""
    );
    assert_eq!(pt(&runner, black), (2, 2));
    assert_eq!(
        hand_size(&runner),
        1,
        "Crystal Spray drew a card after the change"
    );
}

/// Out-of-domain and same-word answers are rejected by the latch rather than applied (CR 608.2d).
#[test]
fn sleight_of_mind_offers_only_color_pairs_and_latches_nothing_for_a_bad_answer() {
    let db = db!();
    let mut scenario = new_scenario();
    let sleight = scenario.add_real_card(P0, "Sleight of Mind", Zone::Hand, db);
    let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    give(&mut runner, P0, &[ManaType::Blue]);

    let outcome = runner.cast(sleight).target_objects(&[wraith]).resolve();
    let WaitingFor::NamedChoice {
        choice_type: ChoiceType::Labeled { options },
        ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!("expected a labeled prompt");
    };
    assert_eq!(options.len(), 20, "only the 20 color pairs");
    assert!(!options.contains(&"Black -> Black".to_string()));
    assert!(!options.contains(&"Forest -> Island".to_string()));
    assert!(
        runner
            .act(GameAction::ChooseOption {
                choice: "Forest -> Island".into()
            })
            .is_err(),
        "a land pair is not offered by a color-only prompt"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "Black -> Blue".into(),
        })
        .expect("a color pair");
}

/// An "until end of turn" change ends at cleanup (CR 514.2).
#[test]
fn crystal_spray_change_ends_at_end_of_turn() {
    let db = db!();
    let mut scenario = new_scenario();
    let spray = scenario.add_real_card(P0, "Crystal Spray", Zone::Hand, db);
    let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    give(
        &mut runner,
        P0,
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
    );

    runner
        .cast(spray)
        .target_objects(&[wraith])
        .choose_option("Swamp -> Forest")
        .resolve();
    assert_eq!(
        walks(&runner, wraith),
        ["Forest"],
        "guard: in effect this turn"
    );
    pass_to_next_turn(&mut runner);
    assert_eq!(walks(&runner, wraith), ["Swamp"], "reverted at cleanup");
}

/// The non-`effect` fields of a resolving ability are rewritten while its runtime state stays bit-identical (CR 612.1).
#[test]
fn restamp_rewrites_repeat_for_and_leaves_runtime_state_alone() {
    let db = db!();
    let mut scenario = new_scenario();
    let sirocco = scenario.add_real_card(P0, "Sirocco", Zone::Hand, db);
    let mut runner = build(scenario, db);

    let def = runner.state().objects[&sirocco].abilities[0].clone();
    let mut ability = build_resolved_from_def(&def, sirocco, P0);
    assert!(
        ability
            .sub_ability
            .as_ref()
            .is_some_and(|sub| sub.repeat_for.is_some()),
        "reach-guard: Sirocco's discard link carries a repeat_for"
    );

    let install = |runner: &mut GameRunner, from: ManaColor, to: ManaColor| {
        let sub = TextSubstitution::color(from, to).unwrap();
        runner.state_mut().add_transient_continuous_effect(
            ObjectId(900),
            P1,
            engine::types::ability::Duration::Permanent,
            engine::types::ability::TargetFilter::SpecificObject { id: sirocco },
            vec![ContinuousModification::SubstituteTextWord {
                substitution: TextSubstitutionSpec::Fixed(sub),
            }],
            None,
        );
    };

    // A word absent from Sirocco leaves the whole ability equal.
    install(&mut runner, ManaColor::Green, ManaColor::Red);
    let before = serde_json::to_value(&ability).unwrap();
    restamp_resolving_spell_text(runner.state(), sirocco, &mut ability);
    assert_eq!(serde_json::to_value(&ability).unwrap(), before);

    install(&mut runner, ManaColor::Blue, ManaColor::Red);
    restamp_resolving_spell_text(runner.state(), sirocco, &mut ability);
    let after = serde_json::to_value(&ability).unwrap();
    let repeat = |value: &serde_json::Value| value.pointer("/sub_ability/repeat_for").cloned();
    assert_ne!(
        repeat(&after),
        repeat(&before),
        "\"blue instant card\" now reads \"red\""
    );
    assert!(repeat(&after).unwrap().to_string().contains("\"Red\""));
    let mut expected = before.clone();
    *expected.pointer_mut("/sub_ability/repeat_for").unwrap() = repeat(&after).unwrap();
    assert_eq!(
        after, expected,
        "only repeat_for moved; controller, source_id, targets and the rest are runtime state"
    );
}

/// The incremental evaluator agrees with a forced full pass under a live substitution, including when the entrant is the recipient (CR 400.7a).
#[test]
fn incremental_flush_matches_full_evaluation_with_a_live_substitution() {
    use std::collections::BTreeSet;
    let db = db!();
    let mut scenario = new_scenario();
    let old = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let entrant = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let unrelated = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);

    let install = |state: &mut engine::types::game_state::GameState,
                   target: ObjectId,
                   from: engine::types::ability::BasicLandType,
                   to: engine::types::ability::BasicLandType| {
        state.add_transient_continuous_effect(
            ObjectId(900),
            P1,
            engine::types::ability::Duration::Permanent,
            engine::types::ability::TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::SubstituteTextWord {
                substitution: TextSubstitutionSpec::Fixed(
                    TextSubstitution::basic_land_type(from, to).unwrap(),
                ),
            }],
            None,
        );
    };
    use engine::types::ability::BasicLandType::{Forest, Plains, Swamp};
    let keywords = |state: &engine::types::game_state::GameState, id: ObjectId| {
        state.objects[&id].keywords.clone()
    };
    let flush_as = |state: &engine::types::game_state::GameState,
                    dirty: engine::types::game_state::LayersDirty| {
        let mut scratch = state.clone();
        scratch.layers_dirty = dirty;
        perf_counters::reset();
        flush_layers(&mut scratch);
        (scratch, perf_counters::snapshot().layers_full_eval)
    };
    use engine::types::game_state::LayersDirty;

    // A live substitution on a pre-existing permanent and an unrelated entrant.
    install(runner.state_mut(), old, Swamp, Plains);
    runner.state_mut().layers_dirty = LayersDirty::Full;
    flush_layers(runner.state_mut());
    let (full, _) = flush_as(runner.state(), LayersDirty::Full);
    assert_eq!(
        walks_in(&full, old),
        ["Plains"],
        "reach-guard: the substitution is live"
    );
    let (incremental, _) = flush_as(
        runner.state(),
        LayersDirty::EnteredObjects(BTreeSet::from([unrelated])),
    );
    for id in [old, entrant, unrelated] {
        assert_eq!(
            keywords(&incremental, id),
            keywords(&full, id),
            "case (a) {id:?}"
        );
    }

    // The entrant is the recipient: the incremental arm would reset it to base
    // and lose the rewrite, so it must escalate to the full pass.
    install(runner.state_mut(), entrant, Swamp, Forest);
    let (full, _) = flush_as(runner.state(), LayersDirty::Full);
    assert_eq!(
        walks_in(&full, entrant),
        ["Forest"],
        "reach-guard: the entrant's change is live"
    );
    let (incremental, full_passes) = flush_as(
        runner.state(),
        LayersDirty::EnteredObjects(BTreeSet::from([entrant])),
    );
    assert!(
        full_passes >= 1,
        "a named entrant escalates the incremental arm"
    );
    assert_eq!(walks_in(&incremental, entrant), ["Forest"]);
    for id in [old, entrant, unrelated] {
        assert_eq!(
            keywords(&incremental, id),
            keywords(&full, id),
            "case (b) {id:?}"
        );
    }
}

/// `active_text_substitutions` groups per recipient and lists each in timestamp order.
#[test]
fn active_substitutions_are_grouped_and_ordered_per_recipient() {
    use engine::types::ability::BasicLandType::{Forest, Plains, Swamp};
    let db = db!();
    let mut scenario = new_scenario();
    let x = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let y = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    for (target, from, to) in [
        (x, Plains, Forest),
        (x, Swamp, Plains),
        (y, Swamp, Forest),
        (y, Swamp, Plains),
    ] {
        runner.state_mut().add_transient_continuous_effect(
            ObjectId(900),
            P1,
            engine::types::ability::Duration::Permanent,
            engine::types::ability::TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::SubstituteTextWord {
                substitution: TextSubstitutionSpec::Fixed(
                    TextSubstitution::basic_land_type(from, to).unwrap(),
                ),
            }],
            None,
        );
    }
    let active = active_text_substitutions(runner.state());
    assert_eq!(active.len(), 2, "one list per recipient");
    assert_eq!(
        active[&x],
        [
            TextSubstitution::basic_land_type(Plains, Forest).unwrap(),
            TextSubstitution::basic_land_type(Swamp, Plains).unwrap(),
        ],
        "X: timestamp order, the routine picks the application order"
    );
    assert_eq!(
        active[&y],
        [
            TextSubstitution::basic_land_type(Swamp, Forest).unwrap(),
            TextSubstitution::basic_land_type(Swamp, Plains).unwrap(),
        ],
        "Y: its own list, untouched by X's chain"
    );
}

const HACK: &str = "Magical Hack";
const SPRAY: &str = "Crystal Spray";
const SLEIGHT: &str = "Sleight of Mind";

fn text_change_mana(card: &str) -> &'static [ManaType] {
    if card == SPRAY {
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless]
    } else {
        &[ManaType::Blue]
    }
}

/// One real text-changing card per `(card, label)` step, in the caster's hand in step order.
fn text_spells(
    scenario: &mut GameScenario,
    db: &engine::database::CardDatabase,
    steps: &[(&str, &str)],
) -> Vec<ObjectId> {
    steps
        .iter()
        .map(|(card, _)| scenario.add_real_card(P0, card, Zone::Hand, db))
        .collect()
}

/// Casts each step at `target`, each resolved before the next is cast.
fn cast_steps(
    runner: &mut GameRunner,
    spells: &[ObjectId],
    steps: &[(&str, &str)],
    target: ObjectId,
) {
    for (spell, (card, label)) in spells.iter().zip(steps) {
        cast_text_change(runner, *spell, target, label, text_change_mana(card));
    }
}

/// The cycle's effects by letter: A = Island -> Swamp, B = Forest -> Island (Magical Hack), C = Swamp -> Forest (Crystal Spray), cast in the order spelled.
fn cycle(order: &str) -> Vec<(&'static str, &'static str)> {
    order
        .chars()
        .map(|step| match step {
            'A' => (HACK, "Island -> Swamp"),
            'B' => (HACK, "Forest -> Island"),
            'C' => (SPRAY, "Swamp -> Forest"),
            other => panic!("unknown cycle step {other}"),
        })
        .collect()
}

/// The type line and offered mana of a real `start` land after the steps resolve this turn.
fn land_after(
    db: &engine::database::CardDatabase,
    start: &str,
    steps: &[(&str, &str)],
) -> (Vec<String>, BTreeSet<ManaType>) {
    let mut scenario = new_scenario();
    let spells = text_spells(&mut scenario, db, steps);
    let land = scenario.add_real_card(P0, start, Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    cast_steps(&mut runner, &spells, steps, land);
    (
        runner.state().objects[&land].card_types.subtypes.clone(),
        offered_mana(&runner, land),
    )
}

/// CR 613.8a-c: the three-cycle on an Island ends Island whichever order it is cast in, because C depends on A and B depends on C.
#[test]
fn land_type_cycle_on_a_type_line_orders_by_the_recipients_current_text() {
    let db = db!();
    for order in ["ABC", "CAB"] {
        assert_eq!(
            land_after(db, "Island", &cycle(order)),
            (vec!["Island".to_string()], BTreeSet::from([ManaType::Blue])),
            "order {order}"
        );
    }
    assert_eq!(
        land_after(db, "Island", &cycle("AB")),
        (vec!["Swamp".to_string()], BTreeSet::from([ManaType::Black])),
        "guard: without C, A and B both resolve and the Island ends a Swamp"
    );
}

/// CR 613.8a-c: the verdict depends on which word the recipient starts with, so each start and order is its own case.
#[test]
fn cycle_verdict_depends_on_the_recipients_starting_land() {
    let db = db!();
    let ty = |name: &str| vec![name.to_string()];
    for (start, order, expected, mana) in [
        ("Forest", "ABC", "Forest", ManaType::Green),
        ("Forest", "CAB", "Swamp", ManaType::Black),
        ("Swamp", "ABC", "Island", ManaType::Blue),
        ("Swamp", "CAB", "Swamp", ManaType::Black),
    ] {
        assert_eq!(
            land_after(db, start, &cycle(order)),
            (ty(expected), BTreeSet::from([mana])),
            "{start}, order {order}"
        );
    }
    assert_eq!(
        land_after(db, "Forest", &cycle("AB")),
        (ty("Swamp"), BTreeSet::from([ManaType::Black])),
        "guard: a Forest with only A and B ends a Swamp"
    );
    assert_eq!(
        land_after(db, "Swamp", &cycle("CB")),
        (ty("Island"), BTreeSet::from([ManaType::Blue])),
        "guard: a Swamp with only C and B ends an Island"
    );
}

/// CR 613.8b: two effects that each remove the Island the other rewrites are a loop and apply in timestamp order, and a later effect that depends on one of them follows.
#[test]
fn type_line_loop_applies_in_timestamp_order() {
    let db = db!();
    let (a, d) = ((HACK, "Island -> Swamp"), (HACK, "Island -> Forest"));
    let c = (SPRAY, "Swamp -> Plains");
    let ty = |name: &str| vec![name.to_string()];
    assert_eq!(
        land_after(db, "Island", &[a, d]),
        (ty("Swamp"), BTreeSet::from([ManaType::Black]))
    );
    assert_eq!(
        land_after(db, "Island", &[d, a]),
        (ty("Forest"), BTreeSet::from([ManaType::Green]))
    );
    assert_eq!(
        land_after(db, "Island", &[a, d, c]),
        (ty("Plains"), BTreeSet::from([ManaType::White])),
        "C depends on A, which made the Swamp C rewrites"
    );
}

/// CR 613.8a: a rewrite onto a land type the type line already holds leaves both instances in the text until every change has applied, so the second change reads two Swamps in either cast order.
#[test]
fn type_line_repeats_count_until_every_change_has_applied() {
    let db = db!();
    let a = (HACK, "Island -> Swamp");
    let b = (HACK, "Swamp -> Forest");
    let ty = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    let line = |steps: &[(&str, &str)]| land_after(db, "Sunken Hollow", steps).0;
    assert_eq!(line(&[a, b]), ty(&["Forest"]));
    assert_eq!(line(&[b, a]), ty(&["Forest"]));
    assert_eq!(line(&[a]), ty(&["Swamp"]), "guard: A alone");
    assert_eq!(line(&[b]), ty(&["Island", "Forest"]), "guard: B alone");
}

/// CR 613.8a-c: a land-type cycle on a keyword recipient applies in dependency order from its current text (Bog Wraith's landwalk).
#[test]
fn land_cycle_on_a_keyword_orders_by_the_recipients_current_text() {
    let db = db!();
    let wraith_after = |order: &str| {
        let steps: Vec<(&str, &str)> = order
            .chars()
            .map(|step| match step {
                'A' => (HACK, "Swamp -> Forest"),
                'B' => (HACK, "Plains -> Swamp"),
                'C' => (SPRAY, "Forest -> Plains"),
                other => panic!("unknown step {other}"),
            })
            .collect();
        let mut scenario = new_scenario();
        let spells = text_spells(&mut scenario, db, &steps);
        let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
        let mut runner = build(scenario, db);
        cast_steps(&mut runner, &spells, &steps, wraith);
        walks(&runner, wraith)
    };
    for order in ["ABC", "CAB"] {
        assert_eq!(wraith_after(order), ["Swamp"], "order {order}");
    }
    assert_eq!(
        wraith_after("AB"),
        ["Forest"],
        "guard: without C, A and B both resolve"
    );
}

/// CR 613.8a-c: the color cycle on a rules-text recipient (Bad Moon) reads from the recipient's current text.
#[test]
fn color_cycle_on_a_static_orders_by_the_recipients_current_text() {
    let db = db!();
    let pts = |order: &str| {
        let steps: Vec<(&str, &str)> = order
            .chars()
            .map(|step| match step {
                'A' => (SLEIGHT, "Black -> Red"),
                'B' => (SLEIGHT, "White -> Black"),
                'C' => (SPRAY, "Red -> White"),
                other => panic!("unknown step {other}"),
            })
            .collect();
        let mut scenario = new_scenario();
        let spells = text_spells(&mut scenario, db, &steps);
        let moon = scenario.add_real_card(P0, "Bad Moon", Zone::Battlefield, db);
        let creatures: Vec<ObjectId> = [ManaColor::Black, ManaColor::White, ManaColor::Red]
            .into_iter()
            .map(|color| {
                scenario
                    .add_creature(P0, "Guy", 2, 2)
                    .with_color(vec![color])
                    .id()
            })
            .collect();
        let mut runner = build(scenario, db);
        cast_steps(&mut runner, &spells, &steps, moon);
        creatures
            .iter()
            .map(|id| runner.state().objects[id].power.unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pts("ABC"),
        [3, 2, 2],
        "black, white, red: the anthem reads Black again"
    );
    assert_eq!(
        pts("AB"),
        [2, 2, 3],
        "guard: without C the anthem reads Red"
    );
}

/// Terror's victim zone after `sleights` resolve against it on the stack, in order.
fn terror_victim_zone(db: &engine::database::CardDatabase, sleights: &[&str]) -> Zone {
    let mut scenario = new_scenario();
    let terror = scenario.add_real_card(P0, "Terror", Zone::Hand, db);
    let spells: Vec<ObjectId> = sleights
        .iter()
        .map(|_| scenario.add_real_card(P0, SLEIGHT, Zone::Hand, db))
        .collect();
    let victim = scenario
        .add_creature(P1, "White Guy", 2, 2)
        .with_color(vec![ManaColor::White])
        .id();
    let mut runner = build(scenario, db);
    give(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);
    let _ = runner.cast(terror).target_objects(&[victim]).commit();
    for (spell, label) in spells.into_iter().zip(sleights) {
        give(&mut runner, P0, &[ManaType::Blue]);
        let _ = runner.cast(spell).target_objects(&[terror]).commit();
        for _ in 0..8 {
            if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
        runner
            .act(GameAction::ChooseOption {
                choice: (*label).into(),
            })
            .expect("answer the word prompt");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: only Terror is left on the stack"
    );
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    runner.state().objects[&victim].zone
}

/// CR 613.8a-c + CR 608.2b: the color cycle cast at a spell on the stack changes what resolves in dependency order, so Terror reads nonblack and still destroys the white creature.
#[test]
fn color_cycle_on_a_stack_spell_orders_by_the_recipients_current_text() {
    let db = db!();
    let (a, b, c) = ("Black -> Red", "White -> Black", "Red -> White");
    assert_eq!(
        terror_victim_zone(db, &[]),
        Zone::Graveyard,
        "guard: unchanged Terror kills the white creature"
    );
    assert_eq!(
        terror_victim_zone(db, &[a, b]),
        Zone::Graveyard,
        "guard: A and B leave Terror nonblack"
    );
    assert_eq!(terror_victim_zone(db, &[a, b, c]), Zone::Graveyard);
}

/// CR 613.8a-c: the restamp seam applies a color cycle in dependency order, so "blue" ends blue.
#[test]
fn restamp_applies_a_color_cycle_in_dependency_order() {
    let db = db!();
    let restamped = |installed: &[(ManaColor, ManaColor)]| {
        let mut scenario = new_scenario();
        let sirocco = scenario.add_real_card(P0, "Sirocco", Zone::Hand, db);
        let mut runner = build(scenario, db);
        let def = runner.state().objects[&sirocco].abilities[0].clone();
        let mut ability = build_resolved_from_def(&def, sirocco, P0);
        let before = serde_json::to_value(&ability).unwrap();
        for &(from, to) in installed {
            runner.state_mut().add_transient_continuous_effect(
                ObjectId(900),
                P1,
                engine::types::ability::Duration::Permanent,
                engine::types::ability::TargetFilter::SpecificObject { id: sirocco },
                vec![ContinuousModification::SubstituteTextWord {
                    substitution: TextSubstitutionSpec::Fixed(
                        TextSubstitution::color(from, to).unwrap(),
                    ),
                }],
                None,
            );
        }
        restamp_resolving_spell_text(runner.state(), sirocco, &mut ability);
        (before, serde_json::to_value(&ability).unwrap())
    };
    let repeat = |value: &serde_json::Value| {
        value
            .pointer("/sub_ability/repeat_for")
            .expect("Sirocco's discard link carries a repeat_for")
            .to_string()
    };
    let a = (ManaColor::Blue, ManaColor::Red);
    let b = (ManaColor::White, ManaColor::Blue);
    let c = (ManaColor::Red, ManaColor::White);

    let (before, after) = restamped(&[a, b]);
    assert!(repeat(&before).contains("\"Blue\""));
    assert!(
        repeat(&after).contains("\"Red\""),
        "guard: A and B turn \"blue\" red"
    );
    let (before, after) = restamped(&[a, b, c]);
    assert_eq!(after, before, "A, C, B returns \"blue\" to blue");
}

fn walks_in(state: &engine::types::game_state::GameState, id: ObjectId) -> Vec<String> {
    state.objects[&id]
        .keywords
        .iter()
        .filter_map(|k| match k {
            Keyword::Landwalk(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Every color or land-type string in the corpus sits at a classified position, and the carrier-directed rewrite round-trips through serde.
#[test]
fn carrier_census_classifies_every_word_position_and_round_trips() {
    let db = db!();
    let mut faces = 0usize;
    let mut word_occurrences = 0usize;
    let mut missing = std::collections::BTreeSet::new();
    let identity = TextSubstitution::color(ManaColor::White, ManaColor::Blue).unwrap();
    for (name, face) in db.face_iter() {
        faces += 1;
        let value = serde_json::json!({
            "abilities": face.abilities,
            "triggers": face.triggers,
            "static_abilities": face.static_abilities,
            "replacements": face.replacements,
            "keywords": face.keywords,
        });
        word_occurrences += classified_word_occurrences(&value);
        for (tag, key, word) in unclassified_word_positions(&value) {
            missing.insert(format!("({tag:?}, {key}) {word} on {name}"));
        }
        // A rewrite that finds nothing is `None`, and one that finds something deserializes back into the same typed shape.
        for ability in &face.abilities {
            if let Some(rewritten) = identity.rewrite(ability) {
                assert_ne!(&rewritten, ability, "{name}");
            }
        }
    }
    assert!(faces > 0, "reach-guard: the census visited faces");
    assert!(
        word_occurrences > 0,
        "reach-guard: the census met classified words"
    );
    assert!(
        missing.is_empty(),
        "unclassified word positions: {missing:#?}"
    );
}

/// The unit surface: labels round-trip, `from == to` and foreign domains are rejected.
#[test]
fn substitution_labels_round_trip_and_reject_invalid_pairs() {
    let both = [TextWordDomain::ColorWord, TextWordDomain::BasicLandType];
    let options = TextSubstitution::options(&both);
    assert_eq!(options.len(), 40);
    for label in &options {
        let sub = TextSubstitution::from_label(label, &both).expect("round trip");
        assert_eq!(&sub.label(), label);
    }
    assert!(TextSubstitution::from_label("Black -> Black", &both).is_none());
    assert!(
        TextSubstitution::from_label("Forest -> Island", &[TextWordDomain::ColorWord]).is_none()
    );
    assert!(TextSubstitution::from_label("Black -> Forest", &both).is_none());
}

/// An entwined Spectral Shift prompts twice, and each mode latches its own answer onto its own target (CR 608.2d).
#[test]
fn entwined_spectral_shift_latches_each_modes_own_answer() {
    let db = db!();
    let mut scenario = new_scenario();
    let shift = scenario.add_real_card(P0, "Spectral Shift", Zone::Hand, db);
    let wraith = scenario.add_real_card(P0, "Bog Wraith", Zone::Battlefield, db);
    let moon = scenario.add_real_card(P0, "Bad Moon", Zone::Battlefield, db);
    let black = scenario
        .add_creature(P0, "Black Guy", 2, 2)
        .with_color(vec![ManaColor::Black])
        .id();
    let white = scenario
        .add_creature(P0, "White Guy", 2, 2)
        .with_color(vec![ManaColor::White])
        .id();
    let mut runner = build(scenario, db);
    give(
        &mut runner,
        P0,
        &[
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );

    let mut committed = runner
        .cast(shift)
        .modes(&[0, 1])
        .target_objects(&[wraith, moon])
        .commit();
    for _ in 0..2 {
        committed
            .act(GameAction::PassPriority)
            .expect("pass priority");
    }
    for answer in ["Swamp -> Island", "Black -> White"] {
        assert!(
            matches!(
                committed.state().waiting_for,
                WaitingFor::NamedChoice { .. }
            ),
            "expected a word prompt before answering {answer}, got {:?}",
            committed.state().waiting_for
        );
        committed
            .act(GameAction::ChooseOption {
                choice: answer.into(),
            })
            .expect("answer the word prompt");
    }
    drop(committed);

    assert_eq!(
        walks(&runner, wraith),
        ["Island"],
        "the land mode hit its own target"
    );
    let pt = |id: ObjectId| {
        let o = &runner.state().objects[&id];
        (o.power.unwrap(), o.toughness.unwrap())
    };
    assert_eq!(
        pt(white),
        (3, 3),
        "the color mode hit Bad Moon: it now pumps white creatures"
    );
    assert_eq!(pt(black), (2, 2));
    assert!(
        runner.state().last_named_choice.is_none(),
        "both answers were consumed"
    );
}
