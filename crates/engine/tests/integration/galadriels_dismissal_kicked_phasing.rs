//! Issue #10266: kicked Galadriel's Dismissal must phase out each creature the
//! targeted player controls, not the targeted player themself.
//!
//! https://github.com/phase-rs/phase/issues/10266

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Galadriel's Dismissal (LTC 500) — verbatim Oracle text.
const GALADRIELS_DISMISSAL: &str = "Kicker {2}{W} (You may pay an additional {2}{W} as you cast \
     this spell.)\nTarget creature phases out. If this spell was kicked, each creature target \
     player controls phases out instead. (Treat phased-out creatures and anything attached to \
     them as though they don't exist until their controller's next turn.)";

fn white_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); n]
}

/// CR 702.26b: only a permanent can phase out. The kicked branch's player
/// target is the controller anchor for "each creature target player controls",
/// so the player must remain phased in while every creature they control
/// phases out. Reverting the `collect_player_targets` player-scope gate makes
/// the player phased out and fails this test.
#[test]
fn kicked_galadriels_dismissal_phases_out_only_the_target_players_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Galadriel's Dismissal", true, GALADRIELS_DISMISSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let caster_creature = scenario.add_creature(P0, "Caster Bear", 2, 2).id();
    let target_creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    let other_target_creature = scenario.add_creature(P1, "Target Elf", 1, 1).id();
    // {W} base plus {2}{W} kicker.
    scenario.with_mana_pool(P0, white_mana(4));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_player(P1)
        .resolve();

    let state = outcome.state();
    assert!(
        !state.players[P0.0 as usize].is_phased_out(),
        "the caster must never phase out"
    );
    assert!(
        !state.players[P1.0 as usize].is_phased_out(),
        "the targeted player is the controller anchor for the creature filter, not a phasing target"
    );
    assert!(
        state.objects[&target_creature].is_phased_out(),
        "the targeted player's first creature must phase out"
    );
    assert!(
        state.objects[&other_target_creature].is_phased_out(),
        "each creature the targeted player controls must phase out"
    );
    assert!(
        state.objects[&caster_creature].is_phased_in(),
        "the caster's creature must remain phased in"
    );
    assert!(
        state.stack.is_empty(),
        "the kicked spell must finish resolving, stack={:?}",
        state.stack.len()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        outcome.final_waiting_for()
    );
}
