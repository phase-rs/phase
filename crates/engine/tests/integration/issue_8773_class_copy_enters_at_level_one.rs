//! Issue #8773: a copy of a Class (Mirrormade on a Class enchantment) could
//! never gain a level, and every level-gated static and replacement on it was
//! inert forever.
//!
//! `GameObject::class_level` is seeded only when an object's own printed face
//! carries the `Class` subtype. A copy gets the subtype and the level bars from
//! the layer system, so its stored level stays absent, and every read site used
//! to fail closed on that absence: the `{Cost}: Level N` gate wanted
//! `Some(N-1)`, so no level was ever reachable.
//!
//! CR 716.2d: "If a rule or effect refers to a permanent's level and that
//! permanent doesn't have a level, it is treated as though its level is 1."
//! CR 716.2b: "Levels are not a copiable characteristic." — so a copy of a
//! level-3 Class is level 1, not level 3.
//! CR 716.2a: "[Cost]: Level N" means "Activate only if this Class is level N-1
//! and only as a sorcery."
//!
//! The fixture puts the ORIGINAL under the opponent and the COPY under P0 so
//! every gated ability under test is discriminating: Innkeeper's Talent's
//! level-2 static ("Permanents *you control* …") and level-3 replacement ("If
//! *you* would put …") are both controller-scoped (CR 109.5), so the opponent's
//! level-3 original can never be the source of an effect observed on P0's side.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, StaticCondition};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::ObjectId;

/// Innkeeper's Talent (BLB), verbatim Oracle text. One card covers all three
/// gated read paths: the level bars (activation restriction), a level-2 static,
/// and a level-3 replacement.
const INNKEEPERS_TALENT: &str = "(Gain the next level as a sorcery to add its ability.)\n\
At the beginning of combat on your turn, put a +1/+1 counter on target creature you control.\n\
{G}: Level 2\n\
Permanents you control with counters on them have ward {1}.\n\
{3}{G}: Level 3\n\
If you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.";

/// Mirrormade (ELD), verbatim Oracle text.
const MIRRORMADE: &str =
    "You may have this enchantment enter as a copy of any artifact or enchantment on the battlefield.";

/// "Put a +1/+1 counter on target creature." — the probe the level-3
/// counter-doubling replacement must modify.
const COUNTER_SPELL_TEXT: &str = "Put a +1/+1 counter on target creature.";

/// Green mana is enough for every cost in this file: Mirrormade is staged
/// cost-free, and both level bars ({G} and {3}{G}) are payable in green.
fn green_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

/// Index of the level bar that sets `level` on `source`.
fn level_bar_index(runner: &GameRunner, source: ObjectId, level: u8) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(
            |a| matches!(a.effect.as_ref(), Effect::SetClassLevel { level: l } if *l == level),
        )
        .unwrap_or_else(|| {
            panic!(
                "the copy must carry the Level {level} bar it copied: {:?}",
                runner.state().objects[&source].abilities
            )
        })
}

/// P1 controls a level-3 Innkeeper's Talent; P0 casts Mirrormade copying it.
/// Returns the runner plus the original and the copy (Mirrormade's own object).
fn copy_of_level_three_talent() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(8));

    // The `Class` subtype must be set BEFORE the Oracle text is parsed:
    // `from_oracle_text` is the call that routes through
    // `parse_class_oracle_text`, and it snapshots the subtypes at call time.
    let original = scenario
        .add_creature(P1, "Innkeeper's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(INNKEEPERS_TALENT)
        .id();

    let mirrormade = scenario
        .add_creature_to_hand(P0, "Mirrormade", 0, 0)
        .as_enchantment()
        .from_oracle_text(MIRRORMADE)
        .id();

    let mut runner = scenario.build();

    // CR 716.2b is only testable against an original that is NOT level 1.
    runner
        .state_mut()
        .objects
        .get_mut(&original)
        .unwrap()
        .class_level = Some(3);

    let outcome = runner
        .cast(mirrormade)
        .accept_optional()
        .replacement_choice(0)
        .copy_target(original)
        .resolve();

    let copy = &outcome.state().objects[&mirrormade];
    assert!(
        copy.card_types.subtypes.iter().any(|s| s == "Class"),
        "reach-guard: Mirrormade must have entered as a copy of the Class \
         (subtypes {:?})",
        copy.card_types.subtypes
    );

    (runner, original, mirrormade)
}

/// CR 716.2d + CR 716.2b: the copy of a level-3 Class is level 1 — it can gain
/// its Level 2, and it cannot skip to Level 3. Before the fix the absent stored
/// level failed the `ClassLevelIs` gate outright, so neither bar was ever legal.
#[test]
fn copy_of_a_level_three_class_enters_at_level_one_and_can_level_up() {
    let (mut runner, original, copy) = copy_of_level_three_talent();

    let level_2 = level_bar_index(&runner, copy, 2);
    let level_3 = level_bar_index(&runner, copy, 3);

    // CR 716.2b: the level is not copied, so the Level 3 bar (gated at level 2)
    // is illegal on a copy of a level-3 Class. Mana is staged well above
    // {3}{G}, so the refusal can only come from the level gate — the paired
    // positive below proves exactly that by activating the SAME bar once the
    // copy has reached level 2.
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: copy,
                ability_index: level_3,
            })
            .is_err(),
        "CR 716.2b: a copy of a level-3 Class must not start at level 3"
    );

    // CR 716.2d + CR 716.2a: absent level reads as 1, so the Level 2 bar is live.
    runner.activate(copy, level_2).resolve();
    assert_eq!(
        runner.state().objects[&copy].class_level,
        Some(2),
        "activating the Level 2 bar must set the copy's level to 2"
    );

    // Paired positive: the same Level 3 bar that was refused above is now legal.
    runner.activate(copy, level_3).resolve();
    assert_eq!(
        runner.state().objects[&copy].class_level,
        Some(3),
        "CR 716.2a: at level 2 the Level 3 bar must be activatable"
    );

    assert_eq!(
        runner.state().objects[&original].class_level,
        Some(3),
        "the original Class must be untouched by the copy's own leveling"
    );
}

/// CR 716.2a + CR 611.3a: the copy's level-2 static ("Permanents you control
/// with counters on them have ward {1}") is off while the copy is level 1 and
/// live once the copy reaches level 2. The opponent's level-3 original controls
/// the same static, but it is scoped to ITS controller (CR 109.5), so ward on
/// P0's creature can only have come from P0's copy.
#[test]
fn copy_level_two_static_applies_once_the_copy_levels_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(8));

    let original = scenario
        .add_creature(P1, "Innkeeper's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(INNKEEPERS_TALENT)
        .id();
    let mirrormade = scenario
        .add_creature_to_hand(P0, "Mirrormade", 0, 0)
        .as_enchantment()
        .from_oracle_text(MIRRORMADE)
        .id();
    let countered_creature = scenario
        .add_creature(P0, "Countered Bear", 2, 2)
        .with_plus_counters(1)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&original)
        .unwrap()
        .class_level = Some(3);
    runner
        .cast(mirrormade)
        .accept_optional()
        .replacement_choice(0)
        .copy_target(original)
        .resolve();

    // Reach-guard: the copied static really is the level-2-gated ward grant.
    // Without this a silent parse failure would make the negative below vacuous.
    assert!(
        runner.state().objects[&mirrormade]
            .static_definitions
            .iter_unchecked()
            .any(|s| s.condition == Some(StaticCondition::ClassLevelGE { level: 2 })),
        "reach-guard: the copy must carry the level-2-gated static: {:?}",
        runner.state().objects[&mirrormade].static_definitions
    );

    let has_ward = |runner: &mut GameRunner| {
        runner.state_mut().layers_dirty.mark_full();
        evaluate_layers(runner.state_mut());
        runner.state().objects[&countered_creature]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Ward(_)))
    };

    assert!(
        !has_ward(&mut runner),
        "CR 716.2a: the copy is level 1, so its level-2 ward grant must be off \
         (and the opponent's level-3 original grants ward only to ITS controller's \
         permanents, CR 109.5)"
    );

    let level_2 = level_bar_index(&runner, mirrormade, 2);
    runner.activate(mirrormade, level_2).resolve();

    assert!(
        has_ward(&mut runner),
        "CR 716.2a: once the copy is level 2 its static must grant ward {{1}} to \
         its controller's permanents with counters"
    );
}

/// CR 716.2a + CR 614.1a: the copy's level-3 replacement ("If you would put one
/// or more counters … put twice that many … instead") is off below level 3 and
/// live at level 3. Same controller-scoping argument as the static test: the
/// opponent's level-3 original doubles only ITS controller's counter placements.
#[test]
fn copy_level_three_replacement_applies_once_the_copy_levels_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(8));

    let original = scenario
        .add_creature(P1, "Innkeeper's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(INNKEEPERS_TALENT)
        .id();
    let mirrormade = scenario
        .add_creature_to_hand(P0, "Mirrormade", 0, 0)
        .as_enchantment()
        .from_oracle_text(MIRRORMADE)
        .id();
    let bear = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let probe_before = scenario
        .add_spell_to_hand_from_oracle(P0, "Counter Probe One", false, COUNTER_SPELL_TEXT)
        .id();
    let probe_after = scenario
        .add_spell_to_hand_from_oracle(P0, "Counter Probe Two", false, COUNTER_SPELL_TEXT)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&original)
        .unwrap()
        .class_level = Some(3);
    runner
        .cast(mirrormade)
        .accept_optional()
        .replacement_choice(0)
        .copy_target(original)
        .resolve();

    // CR 122.1: read the typed +1/+1 counter entry, never a stringly-typed probe.
    let plus_counters = |runner: &GameRunner| {
        runner.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    };

    runner.cast(probe_before).target_objects(&[bear]).resolve();
    assert_eq!(
        plus_counters(&runner),
        1,
        "CR 716.2a: below level 3 the copy must not double counter placement"
    );

    for level in [2u8, 3] {
        let index = level_bar_index(&runner, mirrormade, level);
        runner.activate(mirrormade, index).resolve();
    }
    assert_eq!(
        runner.state().objects[&mirrormade].class_level,
        Some(3),
        "reach-guard: the copy must actually have reached level 3"
    );

    runner.cast(probe_after).target_objects(&[bear]).resolve();
    assert_eq!(
        plus_counters(&runner),
        3,
        "CR 716.2a: at level 3 the copy's replacement must double the second \
         placement (1 + 2), not add a single counter"
    );
}
