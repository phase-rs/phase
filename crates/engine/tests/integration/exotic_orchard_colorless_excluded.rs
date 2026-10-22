//! "{T}: Add one mana of any color that a land an opponent controls could
//! produce." (Exotic Orchard, Fellwar Stone, Sylvok Explorer, Quirion Explorer)
//! ranges over the five colors only.
//!
//! CR 106.1a + CR 105.1: "any color" is white, blue, black, red or green. CR
//! 106.1b: colorless is a mana type, not a color. CR 106.7: the surveyed lands
//! are asked what they could produce; if none could produce a color, there is no
//! type of mana to produce, and CR 106.5 says the ability then produces no mana.
//! The official rulings for Exotic Orchard and Fellwar Stone say it plainly:
//! "can't be tapped for colorless mana, even if a land an opponent controls could
//! produce colorless mana".
//!
//! Card texts below are verbatim Oracle text from `data/mtgjson/AtomicCards.json`
//! (Exotic Orchard, Fellwar Stone, Reflecting Pool, Wastes and Karplusan Forest,
//! checked against that file).

use engine::game::engine::EngineError;
use engine::game::mana_sources::display_land_mana_pips;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaPip, ManaType};
use engine::types::phase::Phase;

const EXOTIC_ORCHARD: &str =
    "{T}: Add one mana of any color that a land an opponent controls could produce.";
const FELLWAR_STONE: &str =
    "{T}: Add one mana of any color that a land an opponent controls could produce.";
const REFLECTING_POOL: &str =
    "{T}: Add one mana of any type that a land you control could produce.";
const WASTES: &str = "{T}: Add {C}.";
const KARPLUSAN_FOREST: &str =
    "{T}: Add {C}.\n{T}: Add {R} or {G}. This land deals 1 damage to you.";

/// The opponent lands a fixture puts on P1's side, in creation order (the
/// engine surveys `state.battlefield` in that order, so a colorless land listed
/// first puts `{C}` first in the pre-fix option set).
#[derive(Clone, Copy)]
enum OpponentLand {
    Wastes,
    Basic(ManaColor),
    KarplusanForest,
}

fn add_opponent_lands(scenario: &mut GameScenario, lands: &[OpponentLand]) {
    for land in lands {
        match land {
            OpponentLand::Wastes => {
                scenario.add_land_from_oracle(P1, "Wastes", WASTES);
            }
            OpponentLand::Basic(color) => {
                scenario.add_basic_land(P1, *color);
            }
            OpponentLand::KarplusanForest => {
                scenario.add_land_from_oracle(P1, "Karplusan Forest", KARPLUSAN_FOREST);
            }
        }
    }
}

/// P0 controls Exotic Orchard; P1 controls `opponent_lands`.
fn orchard_vs(opponent_lands: &[OpponentLand]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    add_opponent_lands(&mut scenario, opponent_lands);
    (scenario.build(), orchard)
}

/// P0 controls Fellwar Stone (an artifact, not a land); P1 controls `opponent_lands`.
fn stone_vs(opponent_lands: &[OpponentLand]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stone = scenario
        .add_artifact_from_oracle(P0, "Fellwar Stone", FELLWAR_STONE)
        .id();
    add_opponent_lands(&mut scenario, opponent_lands);
    (scenario.build(), stone)
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the mana ability");
}

fn pool_total(runner: &GameRunner, count: ManaType) -> (usize, usize) {
    let pool = &runner.state().players[P0.0 as usize].mana_pool;
    (pool.total(), pool.count_color(count))
}

/// The `SingleColor` options of the pending `ChooseManaColor`, sorted, after
/// asserting the prompt is addressed to P0 (the activator).
fn offered_colors(runner: &GameRunner) -> Vec<ManaType> {
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "the activator answers the color prompt");
            let mut offered = options.clone();
            offered.sort();
            offered
        }
        other => panic!("expected a SingleColor ChooseManaColor prompt, got {other:?}"),
    }
}

fn sorted(mut types: Vec<ManaType>) -> Vec<ManaType> {
    types.sort();
    types
}

fn choose(runner: &mut GameRunner, mana_type: ManaType) -> Result<(), EngineError> {
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(mana_type),
            count: 1,
        })
        .map(|_| ())
}

/// V10. A Wastes is the only opponent land: it could produce only {C}, which is
/// not a color, so there is no color to name and the ability produces no mana
/// (CR 106.7 + CR 106.5). The Quirion Explorer ruling: "can still be activated;
/// it just won't produce any mana."
#[test]
fn opponent_wastes_only_produces_no_mana() {
    let (mut runner, orchard) = orchard_vs(&[OpponentLand::Wastes]);
    activate(&mut runner, orchard);

    assert!(
        runner.state().objects[&orchard].tapped,
        "the ability is activatable: its {{T}} cost was paid (CR 602.2b + CR 601.2h)"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "no color exists to choose"
    );
    let (total, colorless) = pool_total(&runner, ManaType::Colorless);
    assert_eq!(
        (total, colorless),
        (0, 0),
        "Exotic Orchard can't be tapped for colorless mana, even if an opponent's land could"
    );

    // Reach guard: the identical fixture with a Forest in place of the Wastes
    // does produce mana, so the zero above is the colorless exclusion and not a
    // dead activation path.
    let (mut control, orchard) = orchard_vs(&[OpponentLand::Basic(ManaColor::Green)]);
    activate(&mut control, orchard);
    assert_eq!(pool_total(&control, ManaType::Green), (1, 1));
}

/// V10 (non-land route). Fellwar Stone is an artifact with the same ability.
#[test]
fn fellwar_stone_facing_only_wastes_produces_no_mana() {
    let (mut runner, stone) = stone_vs(&[OpponentLand::Wastes]);
    activate(&mut runner, stone);

    assert!(runner.state().objects[&stone].tapped);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    assert_eq!(
        pool_total(&runner, ManaType::Colorless),
        (0, 0),
        "Fellwar Stone can't be tapped for colorless mana"
    );

    let (mut control, stone) = stone_vs(&[OpponentLand::Basic(ManaColor::Blue)]);
    activate(&mut control, stone);
    assert_eq!(pool_total(&control, ManaType::Blue), (1, 1));
}

/// V11. A Wastes (created first) and a Forest: green is the only color, so the
/// ability produces {G} with no prompt.
#[test]
fn colorless_and_green_yields_green_without_prompt() {
    let (mut runner, orchard) =
        orchard_vs(&[OpponentLand::Wastes, OpponentLand::Basic(ManaColor::Green)]);
    activate(&mut runner, orchard);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "a single color needs no choice (pre-fix this offered [Colorless, Green])"
    );
    assert_eq!(pool_total(&runner, ManaType::Green), (1, 1));
    assert_eq!(pool_total(&runner, ManaType::Colorless).1, 0);
}

/// V11b. A real painland: Karplusan Forest ({T}: Add {C}. plus {T}: Add {R} or
/// {G}. with a damage rider). The surveyed set is {R, G}; {C} is excluded and an
/// answer of Colorless is rejected (CR 608.2d).
#[test]
fn karplusan_forest_offers_only_its_colors() {
    let (mut runner, orchard) = orchard_vs(&[OpponentLand::KarplusanForest]);

    // Reach guard: the painland's second ability parses as a mana ability, so
    // the {R}/{G} options below come from the real card text.
    let karplusan = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Karplusan Forest")
        .expect("Karplusan Forest is on the battlefield");
    let mana_abilities = runner.state().objects[&karplusan]
        .abilities
        .iter()
        .filter(|ability| engine::game::mana_abilities::is_mana_ability(ability))
        .count();
    assert_eq!(
        mana_abilities, 2,
        "{{C}} and {{R}}/{{G}} are both mana abilities"
    );

    // Display: the Orchard's frame pips keep only the colors.
    let pips = display_land_mana_pips(runner.state(), orchard, P0);
    assert!(
        pips.iter().all(|pip| !matches!(pip, ManaPip::Colorless)),
        "display never shows colorless for the Orchard class: {pips:?}"
    );
    assert!(
        pips.iter().any(|pip| matches!(
            pip,
            ManaPip::OneOfColors(colors)
                if colors.len() == 2
                    && colors.contains(&ManaColor::Red)
                    && colors.contains(&ManaColor::Green)
        )),
        "display offers exactly {{R}}/{{G}}: {pips:?}"
    );

    activate(&mut runner, orchard);
    assert_eq!(
        offered_colors(&runner),
        sorted(vec![ManaType::Red, ManaType::Green]),
        "Karplusan Forest could produce {{C}}, {{R}} or {{G}}; only the colors are offered"
    );
    choose(&mut runner, ManaType::Colorless)
        .expect_err("Colorless is not one of the offered colors");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    choose(&mut runner, ManaType::Green).expect("choose green");
    assert_eq!(pool_total(&runner, ManaType::Green), (1, 1));
}

/// V12. Wastes, Forest and Island: the prompt is exactly {G, U}; Colorless is
/// rejected and Blue is accepted.
#[test]
fn prompt_offers_only_colors() {
    let (mut runner, orchard) = orchard_vs(&[
        OpponentLand::Wastes,
        OpponentLand::Basic(ManaColor::Green),
        OpponentLand::Basic(ManaColor::Blue),
    ]);
    activate(&mut runner, orchard);

    assert_eq!(
        offered_colors(&runner),
        sorted(vec![ManaType::Green, ManaType::Blue])
    );
    assert!(runner.state().objects[&orchard].tapped);

    choose(&mut runner, ManaType::Colorless)
        .expect_err("Colorless is an illegal answer to an any-color choice (CR 608.2d)");
    assert_eq!(pool_total(&runner, ManaType::Colorless).0, 0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "the illegal answer leaves the prompt pending"
    );

    choose(&mut runner, ManaType::Blue).expect("choose blue");
    assert_eq!(pool_total(&runner, ManaType::Blue), (1, 1));
}

fn spell_costing(shards: Vec<ManaCostShard>, generic: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Wastes", WASTES);
    scenario.add_basic_land(P1, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mana Probe", false, "You gain 1 life.")
        .with_mana_cost(ManaCost::Cost { generic, shards })
        .id();
    (scenario.build(), orchard, spell)
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> Result<(), EngineError> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
}

/// V13. Auto-pay must not count the Orchard as a {C} source: a spell costing {C}
/// cannot be paid by an Orchard facing a Wastes and a Forest.
#[test]
fn orchard_cannot_pay_colorless_pip() {
    let (mut runner, orchard, spell) = spell_costing(vec![ManaCostShard::Colorless], 0);
    let result = cast(&mut runner, spell);

    assert!(
        !runner.state().objects[&orchard].tapped,
        "the Orchard must not have been tapped to pay {{C}} (cast result: {result:?})"
    );
    assert_eq!(
        pool_total(&runner, ManaType::Colorless),
        (0, 0),
        "no {{C}} came out of the Orchard"
    );
    let paid_on_stack = runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == spell);
    assert!(
        !paid_on_stack,
        "the {{C}} spell must not have been paid and put on the stack (cast result: {result:?})"
    );
    match result {
        Err(_) => assert_eq!(
            runner.state().objects[&spell].zone,
            engine::types::zones::Zone::Hand,
            "a rejected cast leaves the card in hand"
        ),
        Ok(()) => assert!(
            matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
            "an accepted cast must be waiting on payment, got {:?}",
            runner.state().waiting_for
        ),
    }
}

/// V13 reach guard, an independent runner: the same board pays a `{1}` spell
/// with the Orchard's {G}, so the failure above is about the {C} pip alone.
#[test]
fn orchard_pays_generic_cost_with_a_color() {
    let (mut runner, orchard, spell) = spell_costing(vec![], 1);
    cast(&mut runner, spell).expect("cast the {1} spell");

    assert!(
        runner.state().objects[&orchard].tapped,
        "the Orchard paid for the spell"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell),
        "the paid spell is on the stack"
    );
}

/// V14. Type-sibling guard: Reflecting Pool reads "any **type**" (CR 106.1b), so
/// a Wastes you control still gives it {C}. Fails if the fix is placed in the
/// shared Colorless arm instead of the color-only option set.
#[test]
fn reflecting_pool_still_produces_colorless() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pool = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_land_from_oracle(P0, "Wastes", WASTES);
    let mut runner = scenario.build();
    activate(&mut runner, pool);

    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    assert_eq!(pool_total(&runner, ManaType::Colorless), (1, 1));
}
