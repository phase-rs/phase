//! CR 601.2c + CR 115.4 — "⟨source⟩ deals N damage to each of ⟨count⟩ ⟨noun⟩".
//!
//! Headline card: **Jagged Lightning** ({3}{R}{R} Sorcery, "Jagged Lightning
//! deals 3 damage to each of two target creatures."). Before the parser seam
//! was parameterized across the cardinality × noun matrix, this lowered to
//! `Effect::DamageAll { Fixed(3), Typed[Creature] }` with `multi_target: None`
//! — 3 damage to EVERY creature on the battlefield, in a two-target spell.
//!
//! Every test here drives the real cast pipeline (`GameScenario` +
//! `GameRunner::cast(..).resolve()` + `CastOutcome` deltas) with the card's
//! VERBATIM Oracle text, re-parsed at test time. Reverting the parser change
//! flips T1, T3, T4 and T5 to `DamageAll` and fails them.
//!
//! CR 601.2c: "If the spell has a variable number of targets, the player
//! announces how many targets they will choose… In some cases, the number of
//! targets will be defined by the spell's text."
//! CR 115.4: a bare-plural "two targets" may be creatures, players,
//! planeswalkers, or battles.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::EngineError;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (client/public/card-data.json, verified 2026-07-26).
const JAGGED_LIGHTNING: &str = "Jagged Lightning deals 3 damage to each of two target creatures.";
const PINNACLE_OF_RAGE: &str = "Pinnacle of Rage deals 3 damage to each of two targets.";
const METEOR_BLAST: &str = "Meteor Blast deals 4 damage to each of X targets.";
const FALL_OF_THE_TITANS: &str =
    "Surge {X}{R} (You may cast this spell for its surge cost if you or a teammate has cast \
     another spell this turn.)\nFall of the Titans deals X damage to each of up to two targets.";

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn damage_on(outcome: &engine::game::scenario::CastOutcome, id: ObjectId) -> u32 {
    outcome.state().objects[&id].damage_marked
}

/// T1 — HEADLINE, revert-proof. Three 3/3s on the battlefield; Jagged Lightning
/// targets exactly two of them. The two chosen take 3 each; the third takes
/// NOTHING and survives state-based actions (CR 704.5g).
///
/// Revert behaviour: the pre-fix `DamageAll { Typed[Creature] }` parse hits all
/// three creatures, so `damage_on(bystander)` reads 3 and the bystander is in
/// the graveyard — both assertions below fail.
#[test]
fn t1_jagged_lightning_damages_only_the_two_chosen_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Victim", 3, 3).id();
    let second = scenario.add_creature(P1, "Second Victim", 3, 3).id();
    let bystander = scenario.add_creature(P1, "Bystander", 3, 3).id();
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Jagged Lightning", false, JAGGED_LIGHTNING);
        b.with_mana_cost(ManaCost::generic(0));
        b.id()
    };

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();

    assert_eq!(
        damage_on(&outcome, first),
        3,
        "the first chosen target must take 3"
    );
    assert_eq!(
        damage_on(&outcome, second),
        3,
        "the second chosen target must take 3"
    );
    assert_eq!(
        damage_on(&outcome, bystander),
        0,
        "CR 601.2c: an unchosen creature is not a target and takes no damage — \
         a `DamageAll` parse would read 3 here"
    );
    // CR 704.5g: 3 damage on a 3-toughness creature is lethal. The bystander
    // must still be on the battlefield.
    outcome.assert_zone(&[bystander], Zone::Battlefield);
    outcome.assert_zone(&[first, second], Zone::Graveyard);
}

/// T2′ — MIN-BOUND. `exact(2)` with only ONE legal creature must be rejected:
/// CR 601.2c requires the announced number of targets to be legally choosable.
/// `resolve_multi_target_bounds` raises "Not enough legal targets available"
/// when `legal_target_count < min`.
///
/// This passes trivially on revert (a `DamageAll` parse has no target slots at
/// all), so T1 is its reach-guard: the SAME card, with two creatures available,
/// resolves and marks exactly the two chosen.
#[test]
fn t2_jagged_lightning_rejects_cast_with_only_one_legal_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let only = scenario.add_creature(P1, "Lone Victim", 3, 3).id();
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Jagged Lightning", false, JAGGED_LIGHTNING);
        b.with_mana_cost(ManaCost::generic(0));
        b.id()
    };

    let mut runner = scenario.build();
    let result = runner.cast(spell).target_objects(&[only]).try_resolve();

    match result {
        Err(EngineError::ActionNotAllowed(msg)) => assert!(
            msg.contains("Not enough legal targets"),
            "expected the CR 601.2c minimum-target rejection, got: {msg}"
        ),
        Err(other) => panic!("expected ActionNotAllowed, got: {other:?}"),
        Ok(_) => panic!(
            "casting a two-target spell with only one legal creature must be rejected (CR 601.2c)"
        ),
    }
}

/// T3 — CR 115.4 PLAYER CLASS. Pinnacle of Rage's noun is the BARE plural "two
/// targets", so the target class is creature / player / planeswalker / battle.
/// One creature and one opponent are targeted; both must take 3.
///
/// Revert behaviour: `DamageAll { Any }` is an object-only mass effect and
/// cannot damage a player, so the life delta reads 0.
#[test]
fn t3_pinnacle_of_rage_can_target_a_player_and_a_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Victim", 3, 5).id();
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Pinnacle of Rage", false, PINNACLE_OF_RAGE);
        b.with_mana_cost(ManaCost::generic(0));
        b.id()
    };

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[creature])
        .target_player(P1)
        .resolve();

    outcome.assert_life_delta(P1, -3);
    assert_eq!(
        damage_on(&outcome, creature),
        3,
        "the creature half of the CR 115.4 target class must also take 3"
    );
}

/// T4 — X-COUNT AXIS (mandatory). Meteor Blast is `{X}{R}{R}{R}` and its `X` is
/// the TARGET COUNT, not the damage amount: `multi_target = exact(Variable X)`.
/// Cast for X=2 with three creatures available and two targeted, it must deal 4
/// to each chosen creature, 0 to the third, and complete the cast (not stall at
/// `ChooseXValue` or on the "Target count requires a resolved quantity" guard).
#[test]
fn t4_meteor_blast_x_is_the_target_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Victim", 4, 4).id();
    let second = scenario.add_creature(P1, "Second Victim", 4, 4).id();
    let bystander = scenario.add_creature(P1, "Bystander", 4, 4).id();
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Meteor Blast", false, METEOR_BLAST);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::Red,
                ManaCostShard::Red,
                ManaCostShard::Red,
            ],
            generic: 0,
        });
        b.id()
    };

    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 5);
    let outcome = runner
        .cast(spell)
        .x(2)
        .target_objects(&[first, second])
        .resolve();

    assert_eq!(damage_on(&outcome, first), 4);
    assert_eq!(damage_on(&outcome, second), 4);
    assert_eq!(
        damage_on(&outcome, bystander),
        0,
        "only the X announced targets are damaged"
    );
    outcome.assert_zone(&[bystander], Zone::Battlefield);
}

/// T5 — DECLINING EVERY OPTIONAL SLOT on `up_to(2)`. Fall of the Titans is
/// `{X}{X}{R}` and its `X` is the damage AMOUNT; the count is the fixed literal
/// two, so `collect_target_slots` marks every slot optional (`min = 0`) and
/// `pick_slot_target` declines each one when no object intent is declared.
/// The spell must resolve and deal damage to nobody.
///
/// Reach-guard (anti-vacuity): a SECOND Fall of the Titans in the same fixture,
/// cast for X=3 with one declared target, deals 3 — so "no damage anywhere" in
/// the first cast is a genuine decline, not a dead pipeline.
#[test]
fn t5_fall_of_the_titans_declines_all_optional_target_slots() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 5, 9).id();
    let x_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::X, ManaCostShard::Red],
        generic: 0,
    };
    let declined = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Fall of the Titans",
            false,
            FALL_OF_THE_TITANS,
        );
        b.with_mana_cost(x_cost.clone());
        b.id()
    };
    let targeted = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Fall of the Titans",
            false,
            FALL_OF_THE_TITANS,
        );
        b.with_mana_cost(x_cost);
        b.id()
    };

    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 20);

    let life_before = runner.state().players[1].life;
    let declined_outcome = runner.cast(declined).x(3).resolve();
    assert_eq!(
        damage_on(&declined_outcome, victim),
        0,
        "CR 601.2c: with `up to two` every slot is optional, so declining both \
         deals damage to nothing"
    );
    assert_eq!(
        declined_outcome.state().players[1].life,
        life_before,
        "no player may be damaged either"
    );

    // Reach-guard: the same card, same fixture, with one declared target.
    let targeted_outcome = runner
        .cast(targeted)
        .x(3)
        .target_objects(&[victim])
        .resolve();
    assert_eq!(
        damage_on(&targeted_outcome, victim),
        3,
        "reach-guard: a declared target must take X = 3, proving the declined \
         cast above was a real decline"
    );
}
