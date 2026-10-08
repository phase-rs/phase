//! Issue #6091 — Grindstone repeat loop via `LastZoneChanged` ledger +
//! `ObjectCountBySharedQuality` repeat-until gate.
//!
//! Verbatim Scryfall Oracle (Grindstone, Mirage):
//!
//!   {3}, {T}: Target player mills two cards. If two cards that share a color
//!   were milled this way, repeat this process.
//!
//! Painter's Servant is verified only as combo setup: chosen red must
//! materialize on `obj.color` through layer evaluation before activation.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const GRINDSTONE_ORACLE: &str = "{3}, {T}: Target player mills two cards. If two cards that \
     share a color were milled this way, repeat this process.";

const PAINTER_ORACLE: &str = "As this creature enters, choose a color.\n\
All cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";

fn three_generic() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ),
        ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ),
        ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ),
    ]
}

fn add_colored_library_card(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    shard: ManaCostShard,
) -> engine::types::identifiers::ObjectId {
    scenario
        .add_spell_to_library_top(player, name, true)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![shard],
        })
        .id()
}

fn setup_painter_red(runner: &mut GameRunner, painter: engine::types::identifiers::ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&painter)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));
    evaluate_layers(runner.state_mut());
}

fn assert_objects_include_red(runner: &GameRunner, object_ids: &[ObjectId]) {
    for &id in object_ids {
        let colors = &runner.state().objects.get(&id).unwrap().color;
        assert!(
            colors.contains(&ManaColor::Red),
            "expected {id:?} to include red under Painter, got {colors:?}"
        );
    }
}

fn activate_grindstone_mill_p1(
    runner: &mut GameRunner,
    grindstone: engine::types::identifiers::ObjectId,
) {
    runner.activate(grindstone, 0).target_player(P1).resolve();
}

#[test]
fn grindstone_two_blues_on_top_repeat_without_painter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let grindstone = scenario
        .add_creature_from_oracle(P0, "Grindstone", 0, 0, GRINDSTONE_ORACLE)
        .as_artifact()
        .id();

    add_colored_library_card(&mut scenario, P1, "Blue Top", ManaCostShard::Blue);
    add_colored_library_card(&mut scenario, P1, "Blue Second", ManaCostShard::Blue);
    for idx in 0..4 {
        add_colored_library_card(
            &mut scenario,
            P1,
            &format!("Blue Pad {idx}"),
            ManaCostShard::Blue,
        );
    }
    scenario.with_mana_pool(P0, three_generic());

    let mut runner = scenario.build();

    let lib_before = runner.state().players[P1.0 as usize].library.len();
    activate_grindstone_mill_p1(&mut runner, grindstone);
    let lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = lib_before.saturating_sub(lib_after);

    // Six blue cards: three shared-color iterations × 2, then the library is empty.
    assert_eq!(
        milled, 6,
        "all-blue library must be milled completely through shared-color repeats (milled {milled})"
    );
}

#[test]
fn grindstone_stops_mid_library_when_pair_no_longer_shares_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let grindstone = scenario
        .add_creature_from_oracle(P0, "Grindstone", 0, 0, GRINDSTONE_ORACLE)
        .as_artifact()
        .id();

    // Bottom → top order after successive tops: Blue A, Blue B, Green C, Blue D.
    // First mill: Blue D + Green C (unlike) would stop at 2 — put matching pair on top.
    // Desired top-to-bottom: Blue, Blue, Blue, Green → mill BB (repeat), mill BG (stop) = 4.
    add_colored_library_card(&mut scenario, P1, "Green Stop", ManaCostShard::Green);
    add_colored_library_card(&mut scenario, P1, "Blue Third", ManaCostShard::Blue);
    add_colored_library_card(&mut scenario, P1, "Blue Second", ManaCostShard::Blue);
    add_colored_library_card(&mut scenario, P1, "Blue Top", ManaCostShard::Blue);
    scenario.with_mana_pool(P0, three_generic());

    let mut runner = scenario.build();

    let lib_before = runner.state().players[P1.0 as usize].library.len();
    activate_grindstone_mill_p1(&mut runner, grindstone);
    let lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = lib_before.saturating_sub(lib_after);

    assert_eq!(
        milled, 4,
        "must repeat once on BB then stop on BG (milled {milled})"
    );
}

#[test]
fn grindstone_painter_red_mills_more_than_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTER_ORACLE)
        .as_artifact()
        .id();

    let grindstone = scenario
        .add_creature_from_oracle(P0, "Grindstone", 0, 0, GRINDSTONE_ORACLE)
        .as_artifact()
        .id();

    // Trailing blue pads first; Blue/Green on top so the first mill is the
    // unlike-color pair that only repeats once Painter grants shared red.
    for idx in 0..6 {
        add_colored_library_card(
            &mut scenario,
            P1,
            &format!("Blue Pad {idx}"),
            ManaCostShard::Blue,
        );
    }
    let green_second =
        add_colored_library_card(&mut scenario, P1, "Green Second", ManaCostShard::Green);
    let blue_top = add_colored_library_card(&mut scenario, P1, "Blue Top", ManaCostShard::Blue);

    scenario.with_mana_pool(P0, three_generic());

    let mut runner = scenario.build();
    setup_painter_red(&mut runner, painter);
    assert_objects_include_red(&runner, &[blue_top, green_second]);

    let lib_before = runner.state().players[P1.0 as usize].library.len();
    activate_grindstone_mill_p1(&mut runner, grindstone);
    let lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = lib_before.saturating_sub(lib_after);

    // Eight cards (blue+green + six blue pads), all share red under Painter → full mill.
    assert_eq!(
        milled, 8,
        "Painter red must mill the whole library through shared-color repeats (milled {milled})"
    );
    assert_eq!(
        lib_after, 0,
        "full-library mill must leave no trailing cards"
    );
}

#[test]
fn grindstone_without_painter_stops_at_two_unlike_colors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let grindstone = scenario
        .add_creature_from_oracle(P0, "Grindstone", 0, 0, GRINDSTONE_ORACLE)
        .as_artifact()
        .id();

    for idx in 0..4 {
        add_colored_library_card(
            &mut scenario,
            P1,
            &format!("Blue Trailing {idx}"),
            ManaCostShard::Blue,
        );
    }
    add_colored_library_card(&mut scenario, P1, "Green Second", ManaCostShard::Green);
    add_colored_library_card(&mut scenario, P1, "Blue Top", ManaCostShard::Blue);
    scenario.with_mana_pool(P0, three_generic());

    let mut runner = scenario.build();

    let lib_before = runner.state().players[P1.0 as usize].library.len();
    activate_grindstone_mill_p1(&mut runner, grindstone);
    let lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = lib_before.saturating_sub(lib_after);

    assert_eq!(
        milled, 2,
        "reach guard: blue+green without Painter must not repeat (milled {milled})"
    );
    assert_eq!(
        lib_after, 4,
        "trailing blue cards must remain when the repeat loop does not fire"
    );
}

#[test]
fn grindstone_colorless_pair_stops_at_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let grindstone = scenario
        .add_creature_from_oracle(P0, "Grindstone", 0, 0, GRINDSTONE_ORACLE)
        .as_artifact()
        .id();

    scenario.add_spell_to_library_top(P1, "Colorless Trailing A", true);
    scenario.add_spell_to_library_top(P1, "Colorless Trailing B", true);
    scenario.add_spell_to_library_top(P1, "Colorless B", true);
    scenario.add_spell_to_library_top(P1, "Colorless A", true);
    scenario.with_mana_pool(P0, three_generic());

    let mut runner = scenario.build();

    let lib_before = runner.state().players[P1.0 as usize].library.len();
    activate_grindstone_mill_p1(&mut runner, grindstone);
    let lib_after = runner.state().players[P1.0 as usize].library.len();
    let milled = lib_before.saturating_sub(lib_after);

    assert_eq!(
        milled, 2,
        "two colorless cards produce Max 0 shared-color buckets, so the loop stops at two (milled {milled})"
    );
    assert_eq!(
        lib_after, 2,
        "trailing colorless cards must remain when the repeat loop does not fire"
    );
}
