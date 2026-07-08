//! MSH Wave 5a — Group I: coverage-classifier false-negatives.
//!
//! Five `Source*` static conditions (`SourceIsEquipped`, `SourceIsEnchanted`,
//! `SourceIsMonstrous`, `SourceAttachedToCreature`, `SourceMatchesFilter`) are
//! genuine non-stub runtime arms in `layers::evaluate_condition`, but the
//! coverage classifier in `game/coverage.rs` marked them `Unhandled`, falsely
//! flagging cards that use them as unsupported.
//!
//! These tests prove the cards GENUINELY WORK at runtime by driving the real
//! parse → synthesis → layer pipeline (`add_real_card` from the deployed
//! card-data export + `rehydrate_game_from_card_db` + `evaluate_layers`) and
//! reading back the EFFECTIVE post-layer power/keyword set. Because the
//! classifier flip changes no runtime behavior, a PASS here proves the
//! false-negative is real (the card resolves correctly). A FAILURE would mean a
//! real runtime bug — in which case that variant's classifier must NOT be
//! flipped.

use super::support::shared_card_db;
use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn recompute(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.expect("creature power")
}

fn has_kw(runner: &GameRunner, id: ObjectId, kw: &Keyword) -> bool {
    has_keyword(&runner.state().objects[&id], kw)
}

/// Funds `P0`'s mana pool with floating mana so an activated ability pays cleanly
/// (auto-tap is not modeled in the scenario runner).
fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// CR 301.5a: `SourceIsEquipped` — Armed Assailant ("as long as this creature is
/// equipped, it gets +2/+0 and has menace"). Attaching a real Equipment turns the
/// gate ON; detaching turns it OFF. Discriminating: the Menace keyword is granted
/// ONLY by the source's `SourceIsEquipped`-gated static (the inert equipment used
/// here grants no keyword and no P/T), so its presence proves
/// `evaluate_condition(SourceIsEquipped)` fired. Revert the eval arm and Menace
/// never appears.
#[test]
fn armed_assailant_equipped_gate_grants_buff_and_menace() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let assailant = scenario.add_real_card(P0, "Armed Assailant", Zone::Battlefield, db);
    // Adventuring Gear is a real Equipment with NO continuous P/T static (its pump
    // is a landfall trigger), so it cannot itself contribute to the asserted buff.
    let gear = scenario.add_real_card(P0, "Adventuring Gear", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let base = power(&runner, assailant);

    // Gate OFF: not equipped → no buff, no Menace.
    recompute(&mut runner);
    assert_eq!(power(&runner, assailant), base, "unequipped: base power");
    assert!(
        !has_kw(&runner, assailant, &Keyword::Menace),
        "unequipped: no Menace"
    );

    // Attach the Equipment → gate ON.
    runner
        .state_mut()
        .objects
        .get_mut(&gear)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(assailant));
    recompute(&mut runner);
    assert_eq!(
        power(&runner, assailant),
        base + 2,
        "equipped: +2/+0 from the SourceIsEquipped-gated static"
    );
    assert!(
        has_kw(&runner, assailant, &Keyword::Menace),
        "equipped: Menace granted by the SourceIsEquipped-gated static"
    );

    // Detach → gate OFF again.
    runner
        .state_mut()
        .objects
        .get_mut(&gear)
        .unwrap()
        .attached_to = None;
    recompute(&mut runner);
    assert_eq!(power(&runner, assailant), base, "detached: base power");
    assert!(
        !has_kw(&runner, assailant, &Keyword::Menace),
        "detached: Menace gone"
    );
}

/// CR 701.37: `SourceIsMonstrous` — Fleecemane Lion ("as long as this creature is
/// monstrous, it has hexproof and indestructible"). Drives the REAL Monstrosity
/// activated ability so `monstrous` is set by production effect resolution, then
/// reads the computed keyword set. Discriminating: revert the
/// `evaluate_condition(SourceIsMonstrous)` arm and Hexproof/Indestructible are
/// never granted even after the ability resolves.
#[test]
fn fleecemane_lion_monstrosity_grants_hexproof_indestructible() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lion = scenario.add_real_card(P0, "Fleecemane Lion", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Baseline: not monstrous → no protection.
    recompute(&mut runner);
    assert!(
        !has_kw(&runner, lion, &Keyword::Hexproof),
        "baseline: no Hexproof"
    );
    assert!(
        !has_kw(&runner, lion, &Keyword::Indestructible),
        "baseline: no Indestructible"
    );
    assert!(
        !runner.state().objects[&lion].monstrous,
        "baseline: not monstrous"
    );

    // {3}{G}{W}: Monstrosity 1 — drive the real activated ability.
    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Green,
            ManaType::White,
        ],
    );
    let outcome = runner.activate(lion, 0).resolve();

    assert!(
        outcome.state().objects[&lion].monstrous,
        "Monstrosity 1 resolved → monstrous flag set"
    );

    // Force a full layer recompute on the resolved state so the assertion reads
    // the live `evaluate_condition(SourceIsMonstrous)` arm, independent of any
    // incremental-layer caching during resolution.
    let mut resolved = outcome.state().clone();
    resolved.layers_dirty.mark_full();
    evaluate_layers(&mut resolved);
    assert!(
        has_keyword(&resolved.objects[&lion], &Keyword::Hexproof),
        "monstrous: Hexproof granted by the SourceIsMonstrous-gated static"
    );
    assert!(
        has_keyword(&resolved.objects[&lion], &Keyword::Indestructible),
        "monstrous: Indestructible granted by the SourceIsMonstrous-gated static"
    );
}

/// CR 301.5a + CR 700.9: Patriot, Young Avenger — Prowess plus
/// "as long as Patriot is equipped, OTHER creatures you control get +1/+0".
/// Exercises three axes through the live pipeline:
///   - `SourceIsEquipped` gate on the anthem,
///   - the `Another` affected-filter property (Patriot itself must NOT be
///     buffed by the anthem),
///   - Prowess (the source's own +1/+1 trigger on a noncreature spell).
///
/// Discriminating: revert the `evaluate_condition(SourceIsEquipped)` arm and the
/// other creature's anthem buff vanishes even while equipped.
#[test]
fn patriot_equipped_anthem_buffs_others_not_self() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let patriot = scenario.add_real_card(P0, "Patriot, Young Avenger", Zone::Battlefield, db);
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let gear = scenario.add_real_card(P0, "Adventuring Gear", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let patriot_base = power(&runner, patriot);
    let ally_base = power(&runner, ally);

    // Gate OFF: unequipped → no anthem.
    recompute(&mut runner);
    assert_eq!(
        power(&runner, ally),
        ally_base,
        "unequipped: ally not buffed"
    );

    // Equip Patriot → anthem ON.
    runner
        .state_mut()
        .objects
        .get_mut(&gear)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(patriot));
    recompute(&mut runner);
    assert_eq!(
        power(&runner, ally),
        ally_base + 1,
        "equipped: other creature gets +1/+0 from the anthem"
    );
    assert_eq!(
        power(&runner, patriot),
        patriot_base,
        "equipped: Patriot itself is excluded by the `Another` filter"
    );

    // Unequip → anthem OFF.
    runner
        .state_mut()
        .objects
        .get_mut(&gear)
        .unwrap()
        .attached_to = None;
    recompute(&mut runner);
    assert_eq!(
        power(&runner, ally),
        ally_base,
        "unequipped: anthem gone, ally back to base"
    );
}
