//! Issue #6965: an unparseable subject must not become a board-wide effect.
//!
//! Two production-path regressions, one per half of the fix.
//!
//! **1. Fail closed.** Wand of Orcus — *"Whenever equipped creature attacks or
//! blocks, it and Zombies you control gain deathtouch until end of turn."* Both
//! subject-predicate sites that re-derive a subject used to substitute
//! `TargetFilter::Any` when the subject grammar returned `None`, and
//! `TargetFilter::Any` matches unconditionally (`game/filter.rs`). So the parse
//! FAILURE produced a BOARD-WIDE grant: every permanent the controller had —
//! lands and artifacts included — gained deathtouch. It now produces an honest
//! `Effect::Unimplemented` gap, and the runtime grants nothing.
//!
//! **2. Bind the compound subject.** Lazotep Plating — *"You and permanents you
//! control gain hexproof until end of turn."* This construction used to be
//! recognised by a single hardcoded literal arm matching exactly the phrase
//! `"you" + " and " + "permanents you control"`. It is now parsed by the general
//! CR 611.2c union arm, which parses each conjunct with the ordinary
//! single-subject grammar. This test is the regression guard for deleting the
//! literal arm: it fails if the generalization does not reproduce it.
//!
//! CR 611.2c: one continuous effect naming several subjects determines the set
//! each part applies to independently — i.e. the UNION of the named subjects.
//! CR 301.5f: an Equipment attaches to a creature.
//! CR 702.2b: deathtouch. CR 702.11b: hexproof.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Oracle text (MTGJSON). A paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const WAND_OF_ORCUS: &str = "Whenever equipped creature attacks or blocks, it and Zombies you control gain deathtouch until end of turn.\nWhenever equipped creature deals combat damage to a player, create that many 2/2 black Zombie creature tokens.\nEquip {3}";

/// Verbatim Oracle text (MTGJSON), reminder text included.
const LAZOTEP_PLATING: &str = "Amass Zombies 1. (Put a +1/+1 counter on an Army you control. It's also a Zombie. If you don't control an Army, create a 0/0 black Zombie Army creature token first.)\nYou and permanents you control gain hexproof until end of turn. (You and they can't be the targets of spells or abilities your opponents control.)";

fn keywords(runner: &GameRunner, id: ObjectId) -> Vec<Keyword> {
    runner.state().objects[&id].keywords.clone()
}

#[test]
fn wand_of_orcus_unbindable_subject_grants_nothing_board_wide() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario
        .add_creature(P0, "Wandbearer", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    // The conjunct a PARTIAL union would reach: under an `Or` whose anaphor
    // branch is inert, this Zombie gains deathtouch while the equipped creature
    // does not — a half-applied grant that still reports as supported. That must
    // not happen either.
    let zombie = scenario
        .add_creature(P0, "Shambler", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    let bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    // Not even a creature. Pre-fix, `TargetFilter::Any` reached lands too — the
    // most visible symptom of the fail-open.
    let land = scenario.add_basic_land(P0, ManaColor::Black);

    let wand = scenario
        .add_creature_from_oracle(P0, "Wand of Orcus", 0, 1, WAND_OF_ORCUS)
        .id();

    let mut runner = scenario.build();

    // CR 301.5f: make the Wand a real Equipment attached to `host`, so its
    // "equipped creature attacks" trigger has a subject to fire on.
    {
        let obj = runner.state_mut().objects.get_mut(&wand).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.attached_to = Some(AttachTarget::Object(host));
    }
    evaluate_layers(runner.state_mut());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(host, AttackTarget::Player(P1))])
        .expect("declare the equipped creature as an attacker");

    // Reach-guard: the trigger really did fire and go on the stack. Without it
    // the assertions below would pass vacuously on a card that never triggered.
    assert_eq!(
        runner.stack_names(),
        vec!["Wand of Orcus".to_string()],
        "the attack trigger must be on the stack, or nothing below is exercised"
    );

    runner.advance_until_stack_empty();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // The printed subject ("it and Zombies you control") carries an anaphor
    // conjunct the union cannot bind, so the whole clause fails closed. Nothing
    // is granted — most importantly, NOT everything.
    for (id, label) in [
        (host, "the equipped creature"),
        (zombie, "a Zombie you control"),
        (bear, "an unrelated creature you control"),
        (land, "a LAND you control"),
    ] {
        assert!(
            !keywords(&runner, id).contains(&Keyword::Deathtouch),
            "{label} must not gain deathtouch: the printed subject could not be \
             bound, so the clause is an honest gap (issue #6965 — it used to \
             become TargetFilter::Any and grant to every permanent)"
        );
    }
}

#[test]
fn lazotep_plating_grants_hexproof_to_the_union_of_both_conjuncts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ally = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    // The second conjunct is "permanents you control", not "creatures" — a land
    // you control must be covered too.
    let ally_land = scenario.add_basic_land(P0, ManaColor::White);
    // Excluded by "you control".
    let foe = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let plating = scenario
        .add_spell_to_hand_from_oracle(P0, "Lazotep Plating", true, LAZOTEP_PLATING)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(plating).resolve();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // CR 611.2c: both named subjects are covered.
    assert!(
        keywords(&runner, ally).contains(&Keyword::Hexproof),
        "a creature you control is inside \"permanents you control\" and must \
         gain hexproof"
    );
    assert!(
        keywords(&runner, ally_land).contains(&Keyword::Hexproof),
        "a LAND you control is a permanent you control and must gain hexproof"
    );
    // The negative arm is non-vacuous: the two positives above prove the grant
    // resolved at all.
    assert!(
        !keywords(&runner, foe).contains(&Keyword::Hexproof),
        "an opponent's permanent is excluded by \"you control\""
    );
}
