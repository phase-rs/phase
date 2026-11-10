//! Issue #10266: kicked Galadriel's Dismissal must phase out each creature the
//! targeted player controls, not the targeted player themself.
//!
//! https://github.com/phase-rs/phase/issues/10266

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Galadriel's Dismissal (LTC 500) — verbatim Oracle text.
const GALADRIELS_DISMISSAL: &str = "Kicker {2}{W} (You may pay an additional {2}{W} as you cast \
     this spell.)\nTarget creature phases out. If this spell was kicked, each creature target \
     player controls phases out instead. (Treat phased-out creatures and anything attached to \
     them as though they don't exist until their controller's next turn.)";

/// Bolt Bend — verbatim Oracle text.
const BOLT_BEND: &str = "This spell costs {3} less to cast if you control a creature with power 4 \
     or greater.\nChange the target of target spell or ability with a single target.";

/// Teferi, Timeless Voyager — verbatim Oracle text.
const TEFERI_TIMELESS_VOYAGER: &str =
    "[+1]: Draw a card.\n[−3]: Put target creature on top of its \
     owner's library.\n[−8]: Each creature target opponent controls phases out. Until the end of \
     your next turn, they can't phase in.";

fn white_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); n]
}

fn red_mana(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); n]
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

/// CR 115.7a + CR 115.7d: with the kicked branch reduced to its announced
/// player target, Bolt Bend can target the spell and must be offered the
/// player pool — never the unkicked base branch's creature pool. Retargeting
/// to the caster phases out the caster's creatures instead.
#[test]
fn kicked_galadriels_dismissal_retargets_through_the_player_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dismissal = scenario
        .add_spell_to_hand_from_oracle(P0, "Galadriel's Dismissal", true, GALADRIELS_DISMISSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P1, "Bolt Bend", true, BOLT_BEND)
        .id();
    let caster_creature = scenario.add_creature(P0, "Caster Bear", 2, 2).id();
    let target_creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    scenario.with_mana_pool(P0, white_mana(4));
    scenario.with_mana_pool(P1, red_mana(4));

    let mut runner = scenario.build();
    runner
        .cast(dismissal)
        .accept_optional()
        .target_player(P1)
        .commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 may respond");

    let outcome = runner.cast(bolt_bend).target_object(dismissal).resolve();
    let WaitingFor::RetargetChoice {
        legal_new_targets,
        slot_pools,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "Bolt Bend must present the retarget choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(
        legal_new_targets.contains(&TargetRef::Player(P0)),
        "the caster must be a legal replacement player, got {legal_new_targets:?}"
    );
    assert!(
        !legal_new_targets
            .iter()
            .any(|target| matches!(target, TargetRef::Object(_))),
        "the base branch's creature pool must not leak into the player slot: {legal_new_targets:?}"
    );
    assert!(
        slot_pools
            .first()
            .is_some_and(|pool| pool.contains(&TargetRef::Player(P0))),
        "the per-slot pool must be the companion player pool, got {slot_pools:?}"
    );

    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Player(P0))],
        })
        .expect("retarget the kicked spell to the caster");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert!(
        state.objects[&caster_creature].is_phased_out(),
        "the caster's creature is now the affected population"
    );
    assert!(
        state.objects[&target_creature].is_phased_in(),
        "the original target's creature must stay phased in"
    );
    assert!(
        !state.players[P0.0 as usize].is_phased_out(),
        "the retargeted player is still only the population anchor"
    );
}

/// CR 702.26b + CR 101.2: Teferi's follow-on "they can't phase in" must bind
/// the creatures that actually phased out, not the player-only anchor. At the
/// opponent's next untap step they remain phased out while the restriction is
/// active.
#[test]
fn teferi_timeless_voyager_restriction_binds_the_phased_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Teferi, Timeless Voyager",
            "Teferi",
            8,
            TEFERI_TIMELESS_VOYAGER,
        )
        .id();
    let first_opponent_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let second_opponent_creature = scenario.add_creature(P1, "Opponent Elf", 1, 1).id();
    let own_creature = scenario.add_creature(P0, "Own Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.activate(teferi, 2).target_player(P1).resolve();

    assert!(
        runner.state().objects[&first_opponent_creature].is_phased_out(),
        "the opponent's first creature must phase out"
    );
    assert!(
        runner.state().objects[&second_opponent_creature].is_phased_out(),
        "the opponent's second creature must phase out"
    );
    assert!(
        !runner.state().players[P1.0 as usize].is_phased_out(),
        "the targeted opponent is the population anchor, not a phasing target"
    );
    assert!(
        runner.state().objects[&own_creature].is_phased_in(),
        "the controller's own creature must remain phased in"
    );

    // Advance to the opponent's untap step: the phased-out creatures would
    // normally phase in here, but Teferi's active restriction holds them out.
    runner.advance_to_phase(Phase::Untap);
    assert!(
        runner.state().objects[&first_opponent_creature].is_phased_out(),
        "the restriction must be bound to the first phased creature"
    );
    assert!(
        runner.state().objects[&second_opponent_creature].is_phased_out(),
        "the restriction must be bound to every phased creature during the opponent's untap"
    );
    assert!(
        runner.state().objects[&own_creature].is_phased_in(),
        "Teferi's restriction must not affect the controller's creature"
    );
}
