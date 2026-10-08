//! Issue #6507: the depletion-land sacrifice rider never fires (Gemstone Mine
//! class). "{T}, Remove a mining counter from this land: Add one mana of any
//! color. If there are no mining counters on this land, sacrifice it." — the
//! rider's bare "it" mis-bound to `ParentTarget` at parse time; a mana ability
//! has no targets (CR 605.1a), so the sacrifice sub-chain silently no-oped and
//! the land survived with zero counters.
//!
//! Fix: `condition_refs_source_object` now recognizes a source-scoped counter
//! `QuantityCheck` (CR 122.1 + CR 608.2k), so the chunk subject threads
//! `SelfRef` and `resolve_it_pronoun` binds "sacrifice it" to the source.
//!
//! Discriminators (flip when the fix is reverted):
//! - last-counter activations leave the land on the battlefield instead of in
//!   the graveyard (tests 1, 3, 4);
//! - the typed-subject trigger sibling (Last Light of Durin's Day shape)
//!   sacrifices the TRIGGERING Mountain instead of the source enchantment
//!   (test 5 — both zone assertions flip).

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::parse_counter_type;
use engine::types::game_state::{ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle texts (card-data.json, verified 2026-07-23).
const GEMSTONE_MINE_ORACLE: &str = "This land enters with three mining counters on it.\n{T}, Remove a mining counter from this land: Add one mana of any color. If there are no mining counters on this land, sacrifice it.";
const PEAT_BOG_ORACLE: &str = "This land enters tapped with two depletion counters on it.\n{T}, Remove a depletion counter from this land: Add {B}{B}. If there are no depletion counters on this land, sacrifice it.";
const DARK_RITUAL_ORACLE: &str = "Add {B}{B}{B}.";
// Trigger line of Last Light of Durin's Day (the Mountaincycling keyword line
// is irrelevant to the rider under test and omitted).
const LAST_LIGHT_ORACLE: &str = "Whenever a Mountain you control enters, put a quest counter on this enchantment. If it has six or more quest counters on it, sacrifice it. If you do, search your hand and/or library for a Dragon card and put it onto the battlefield. If you search your library this way, shuffle.";
const REVELATION_OF_POWER_ORACLE: &str = "Target creature gets +2/+2 until end of turn. If it has a counter on it, it also gains flying and lifelink until end of turn.";

fn counter_count(runner: &engine::game::scenario::GameRunner, id: ObjectId, counter: &str) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&parse_counter_type(counter))
        .copied()
        .unwrap_or(0)
}

fn pool_count(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    mana: ManaType,
) -> usize {
    runner.state().players[player.0 as usize]
        .mana_pool
        .count_color(mana)
}

fn gemstone_mine_scenario(mining_counters: u32) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_from_oracle(P0, "Gemstone Mine", GEMSTONE_MINE_ORACLE)
        .id();
    // Scenario-seeded permanents skip ETB replacements, so seed the counters
    // directly (the real card enters with three).
    scenario.with_counter(land, parse_counter_type("mining"), mining_counters);
    (scenario.build(), land)
}

fn activate_and_choose_green(runner: &mut engine::game::scenario::GameRunner, land: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 0,
        })
        .expect("Gemstone Mine's mana ability must be payable while a mining counter remains");
    // CR 605.3b: the mana ability resolves immediately after the color choice —
    // no stack, no priority round-trip.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "AnyOneColor production must prompt for a color, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("choose green");
}

/// Test 1 (discriminating): removing the LAST mining counter must sacrifice
/// Gemstone Mine after the mana is produced. Pre-fix the rider's `ParentTarget`
/// matched nothing (a mana ability has no targets, CR 605.1a) and the land
/// wrongly stayed on the battlefield.
#[test]
fn gemstone_mine_last_counter_sacrifices_after_mana() {
    let (mut runner, land) = gemstone_mine_scenario(1);
    activate_and_choose_green(&mut runner, land);

    // Reach-guards: the ability resolved — mana was produced and the cost's
    // counter removal happened — so the zone assertion is not vacuous.
    assert_eq!(
        pool_count(&runner, P0, ManaType::Green),
        1,
        "the mana ability must resolve and add one green mana"
    );
    assert_eq!(
        counter_count(&runner, land, "mining"),
        0,
        "the activation cost must have removed the last mining counter"
    );
    // CR 701.21a: the rider sacrifices the land itself.
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "with no mining counters left, Gemstone Mine must sacrifice itself"
    );
}

/// Test 2 (negative sibling): with a counter remaining the gate is false and
/// the land stays. The positive pair (mana produced + exactly one counter
/// left) proves resolution reached the gate and the gate evaluated false —
/// not that resolution never ran.
#[test]
fn gemstone_mine_two_counters_stays_on_battlefield() {
    let (mut runner, land) = gemstone_mine_scenario(2);
    activate_and_choose_green(&mut runner, land);

    assert_eq!(
        pool_count(&runner, P0, ManaType::Green),
        1,
        "the mana ability must resolve and add one green mana"
    );
    assert_eq!(
        counter_count(&runner, land, "mining"),
        1,
        "one activation must remove exactly one mining counter"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "with a mining counter remaining, the sacrifice gate is false and the land stays"
    );
}

/// Test 3 (class sibling, discriminating): Peat Bog — different counter type,
/// fixed multi-mana production, no color prompt. Removing the last depletion
/// counter must sacrifice the land.
#[test]
fn peat_bog_last_depletion_counter_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_from_oracle(P0, "Peat Bog", PEAT_BOG_ORACLE)
        .id();
    scenario.with_counter(land, parse_counter_type("depletion"), 1);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 0,
        })
        .expect("Peat Bog's mana ability must be payable while a depletion counter remains");

    // Reach-guard: {B}{B} landed in the pool, so the ability resolved.
    assert_eq!(
        pool_count(&runner, P0, ManaType::Black),
        2,
        "Peat Bog must add {{B}}{{B}}"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "with no depletion counters left, Peat Bog must sacrifice itself"
    );
}

/// Test 4 (auto-tap payment path, discriminating): casting a {B} spell with
/// Peat Bog (one depletion counter) as the only mana source. Auto payment taps
/// Peat Bog, removing its last counter; the rider must sacrifice it during
/// payment. Pre-fix the spell resolved but the land wrongly survived.
#[test]
fn peat_bog_autotap_payment_sacrifices_after_last_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_from_oracle(P0, "Peat Bog", PEAT_BOG_ORACLE)
        .id();
    scenario.with_counter(land, parse_counter_type("depletion"), 1);
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Dark Ritual", true, DARK_RITUAL_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        });
        b.id()
    };
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    // Reach-guard: the cast and its payment executed — Peat Bog produced
    // {B}{B}, one paid the cost, and Dark Ritual's resolution added {B}{B}{B},
    // leaving four black mana in the pool.
    assert_eq!(
        outcome
            .state()
            .players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Black),
        4,
        "auto-tap must pay {{B}} from Peat Bog's {{B}}{{B}} and Dark Ritual must add {{B}}{{B}}{{B}}"
    );
    outcome.assert_zone(&[spell], Zone::Graveyard);
    // CR 701.21a: the rider fires on the auto-tap path too — the sub-chain runs
    // inside `resolve_mana_ability` regardless of how the activation happened.
    assert_eq!(
        outcome.zone_of(land),
        Zone::Graveyard,
        "paying with Peat Bog's last depletion counter must sacrifice it"
    );
}

/// Test 5 (multi-authority hostile, discriminating): the typed-subject trigger
/// sibling. Two live candidate objects — the triggering Mountain and the
/// source enchantment. Pre-fix the rider bound to `TriggeringSource` and
/// sacrificed the MOUNTAIN while the enchantment survived; post-fix the
/// enchantment is sacrificed and the Mountain stays (CR 608.2k — "it" refers
/// to the source named by the rider's own condition).
#[test]
fn source_counter_rider_sacrifices_source_not_triggering_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let enchantment = {
        let mut b = scenario.add_creature(P0, "Last Light of Durin's Day", 0, 0);
        b.as_enchantment().from_oracle_text(LAST_LIGHT_ORACLE);
        b.id()
    };
    scenario.with_counter(enchantment, parse_counter_type("quest"), 5);
    let mountain = {
        let mut b = scenario.add_land_to_hand(P0, "Mountain");
        b.with_subtypes(vec!["Mountain"]);
        b.id()
    };
    // A findable Dragon in the library so the post-sacrifice "search your hand
    // and/or library for a Dragon card" surfaces a mandatory `SearchChoice`
    // prompt — the halt point the trigger only reaches if the sacrifice was
    // performed ("If you do"). Without a findable Dragon the search silently
    // fails to find and the trigger fully resolves, so there is no clean
    // boundary to inspect.
    {
        let mut b = scenario.add_spell_to_library_top(P0, "Shivan Dragon", false);
        b.as_creature().with_subtypes(vec!["Dragon"]);
    }
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&mountain].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: mountain,
            card_id,
        })
        .expect("play the Mountain from hand");

    // Drain priority until the trigger resolves; it halts at the mandatory
    // "search your hand and/or library" prompt (the /card-test boundary rule).
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    // Reach-guard: the trigger body ran through the counter placement AND the
    // sacrifice gate — the "If you do" search prompt only surfaces after a
    // sacrifice was performed, so neither zone assertion below is vacuous.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "the post-sacrifice search prompt must surface, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&enchantment].zone,
        Zone::Graveyard,
        "the rider must sacrifice the SOURCE enchantment (sixth quest counter reached)"
    );
    assert_eq!(
        runner.state().objects[&mountain].zone,
        Zone::Battlefield,
        "the triggering Mountain must survive — pre-fix the TriggeringSource \
         mis-binding sacrificed it instead of the source"
    );
}

/// Issue #6559 regression: the intervening counter check reads the previously
/// chosen creature, not the Instant. This drives casting, target selection,
/// condition evaluation, and layer application; it fails if the source-counter
/// guard binds the rider's bare pronouns to `SelfRef`.
#[test]
fn revelation_of_power_grants_keywords_to_its_countered_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Countered Creature", 2, 2).id();
    scenario.with_counter(creature, engine::types::counter::CounterType::Plus1Plus1, 1);
    let revelation = scenario
        .add_spell_to_hand_from_oracle(P0, "Revelation of Power", true, REVELATION_OF_POWER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(revelation).target_object(creature).resolve();

    assert!(
        outcome.state().objects[&creature].has_keyword(&Keyword::Flying),
        "the countered target must gain flying"
    );
    assert!(
        outcome.state().objects[&creature].has_keyword(&Keyword::Lifelink),
        "the countered target must gain lifelink"
    );
}
